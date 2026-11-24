"""Bounded CUDA execution for one experimental EAGW-PoW-V2 ticket search."""

import argparse
import ctypes as c
import hashlib
import json
import multiprocessing
import os
from pathlib import Path
import struct
import time
import stat

from eagw_gpu_once import (
    CUDA_PATH,
    NVRTC_BUILTINS_PATH,
    NVRTC_PATH,
    check,
    save,
    verify_runtime_libraries,
)

VERSION = 2
INPUT_DOMAIN = b"EAGW-POW-V2-INPUT\0"
COMMIT_DOMAIN = b"EAGW-POW-V2-COMMIT\0"
TICKET_DOMAIN = b"EAGW-POW-V2-TICKET\0"
KERNEL = b'''extern "C" __global__ void matrix(const int* in, int* out) {
 int i=threadIdx.x; if(i<32) { int row=i/16, col=i%16, sum=0;
 for(int k=0;k<2048;k++) sum += in[row*2048+k]*in[(2+col)*2048+k];
 out[i]=sum; }}'''
ELEMENTS = 18 * 2048
INPUT_BYTES = ELEMENTS * 4
OUTPUT_BYTES = 32 * 4


def runtime_libraries():
    """Use the original WSL pins, or an explicit pinned remote CUDA manifest."""
    manifest_path = os.environ.get("EAGW_POW_LIBRARY_MANIFEST")
    if not manifest_path:
        verify_runtime_libraries()
        return CUDA_PATH, NVRTC_PATH, NVRTC_BUILTINS_PATH, (8, 6)
    path = Path(manifest_path)
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise RuntimeError("pinned CUDA manifest must be an absolute regular file")
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if set(manifest) != {"cuda", "nvrtc", "builtins", "compute_capability"}:
        raise RuntimeError("invalid pinned CUDA manifest shape")
    capability = manifest["compute_capability"]
    if capability not in ("8.6", "12.0"):
        raise RuntimeError("unsupported pinned GPU capability")
    libraries = []
    for name in ("cuda", "nvrtc", "builtins"):
        entry = manifest[name]
        if set(entry) != {"path", "sha256"}:
            raise RuntimeError(f"invalid {name} library pin")
        library = Path(entry["path"])
        expected = entry["sha256"]
        if (not library.is_absolute() or library.is_symlink()
                or not library.is_file() or not stat.S_ISREG(library.stat().st_mode)
                or len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected)):
            raise RuntimeError(f"invalid {name} library path or hash")
        digest = hashlib.sha256()
        with library.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != expected:
            raise RuntimeError(f"{name} library pin mismatch")
        libraries.append(library)
    return *libraries, tuple(map(int, capability.split(".")))


def canonical_gpu_uuid(raw):
    value = bytes(raw).hex()
    return f"GPU-{value[:8]}-{value[8:12]}-{value[12:16]}-{value[16:20]}-{value[20:]}"


def valid_canonical_gpu_uuid(value):
    if len(value) != 40 or not value.startswith("GPU-"):
        return False
    for index, char in enumerate(value):
        if index in (3, 12, 17, 22, 27):
            if char != "-":
                return False
        elif index < 3:
            if char != "GPU"[index]:
                return False
        elif char not in "0123456789abcdef":
            return False
    return True


def input_bytes(challenge, nonce):
    stream = b"".join(
        hashlib.blake2s(
            INPUT_DOMAIN
            + challenge
            + struct.pack("<Q", nonce)
            + struct.pack("<I", counter)
        ).digest()
        for counter in range(ELEMENTS // 32)
    )
    return b"".join(struct.pack("<i", byte % 129 - 64) for byte in stream)


def ticket(challenge, nonce, input_digest, output_digest):
    nonce_bytes = struct.pack("<Q", nonce)
    commitment = hashlib.sha256(
        COMMIT_DOMAIN + challenge + nonce_bytes + input_digest + output_digest
    ).digest()
    return hashlib.sha256(
        TICKET_DOMAIN
        + challenge
        + nonce_bytes
        + input_digest
        + output_digest
        + commitment
    ).digest()


def worker(challenge, target, start_nonce, max_attempts, out, deadline, expected_gpu_uuid,
           on_ready=None, on_candidate=None):
    cuda_path, nvrtc_path, builtins_path, expected_capability = runtime_libraries()
    builtins = c.CDLL(str(builtins_path), mode=os.RTLD_NOW | os.RTLD_GLOBAL)
    cuda = c.CDLL(str(cuda_path), mode=os.RTLD_NOW | os.RTLD_LOCAL)
    nvrtc = c.CDLL(str(nvrtc_path), mode=os.RTLD_NOW | os.RTLD_LOCAL)
    cuda_signatures = {
        "cuInit": [c.c_uint],
        "cuDeviceGet": [c.POINTER(c.c_int), c.c_int],
        "cuDeviceGetUuid": [c.c_void_p, c.c_int],
        "cuDeviceComputeCapability": [c.POINTER(c.c_int), c.POINTER(c.c_int), c.c_int],
        "cuCtxCreate_v2": [c.POINTER(c.c_void_p), c.c_uint, c.c_int],
        "cuCtxDestroy_v2": [c.c_void_p],
        "cuModuleLoadData": [c.POINTER(c.c_void_p), c.c_void_p],
        "cuModuleUnload": [c.c_void_p],
        "cuModuleGetFunction": [c.POINTER(c.c_void_p), c.c_void_p, c.c_char_p],
        "cuMemAlloc_v2": [c.POINTER(c.c_uint64), c.c_size_t],
        "cuMemFree_v2": [c.c_uint64],
        "cuMemcpyHtoD_v2": [c.c_uint64, c.c_void_p, c.c_size_t],
        "cuMemcpyDtoH_v2": [c.c_void_p, c.c_uint64, c.c_size_t],
        "cuLaunchKernel": [c.c_void_p] + [c.c_uint] * 7 + [c.c_void_p, c.POINTER(c.c_void_p), c.c_void_p],
        "cuCtxSynchronize": [],
    }
    for name, args in cuda_signatures.items():
        fn = getattr(cuda, name)
        fn.argtypes = args
        fn.restype = c.c_int
    nvrtc_signatures = {
        "nvrtcCreateProgram": [c.POINTER(c.c_void_p), c.c_char_p, c.c_char_p, c.c_int, c.c_void_p, c.c_void_p],
        "nvrtcCompileProgram": [c.c_void_p, c.c_int, c.POINTER(c.c_char_p)],
        "nvrtcGetPTXSize": [c.c_void_p, c.POINTER(c.c_size_t)],
        "nvrtcGetPTX": [c.c_void_p, c.c_void_p],
        "nvrtcDestroyProgram": [c.POINTER(c.c_void_p)],
    }
    for name, args in nvrtc_signatures.items():
        fn = getattr(nvrtc, name)
        fn.argtypes = args
        fn.restype = c.c_int

    context = c.c_void_p()
    module = c.c_void_p()
    program = c.c_void_p()
    device_input = c.c_uint64()
    device_output = c.c_uint64()
    cleanup_errors = []
    winner = None
    attempts = 0
    kernel_seconds = 0.0
    try:
        check(cuda.cuInit(0))
        device = c.c_int()
        check(cuda.cuDeviceGet(c.byref(device), 0))
        major, minor = c.c_int(), c.c_int()
        check(cuda.cuDeviceComputeCapability(c.byref(major), c.byref(minor), device))
        if (major.value, minor.value) != expected_capability:
            raise RuntimeError(f"device must be compute capability {expected_capability}")
        uuid = (c.c_ubyte * 16)()
        check(cuda.cuDeviceGetUuid(uuid, device))
        actual_gpu_uuid = canonical_gpu_uuid(uuid)
        if actual_gpu_uuid != expected_gpu_uuid:
            raise RuntimeError(
                f"CUDA ordinal 0 UUID mismatch: expected {expected_gpu_uuid}, observed {actual_gpu_uuid}"
            )
        check(cuda.cuCtxCreate_v2(c.byref(context), 0, device))
        check(nvrtc.nvrtcCreateProgram(c.byref(program), KERNEL, b"eagw-pow-v2.cu", 0, None, None))
        architecture = f"--gpu-architecture=compute_{major.value}{minor.value}".encode("ascii")
        options = (c.c_char_p * 1)(architecture)
        check(nvrtc.nvrtcCompileProgram(program, 1, options))
        ptx_size = c.c_size_t()
        check(nvrtc.nvrtcGetPTXSize(program, c.byref(ptx_size)))
        if not 0 < ptx_size.value <= 16 * 1024 * 1024:
            raise RuntimeError("invalid PTX size")
        ptx = c.create_string_buffer(ptx_size.value)
        check(nvrtc.nvrtcGetPTX(program, ptx))
        check(cuda.cuModuleLoadData(c.byref(module), ptx))
        kernel = c.c_void_p()
        check(cuda.cuModuleGetFunction(c.byref(kernel), module, b"matrix"))
        check(cuda.cuMemAlloc_v2(c.byref(device_input), INPUT_BYTES))
        check(cuda.cuMemAlloc_v2(c.byref(device_output), OUTPUT_BYTES))
        if on_ready is not None:
            on_ready()
        for offset in range(max_attempts):
            if time.monotonic() >= deadline:
                break
            nonce = start_nonce + offset
            if nonce > 0xFFFFFFFFFFFFFFFF:
                break
            raw_input = input_bytes(challenge, nonce)
            host_input = c.create_string_buffer(raw_input)
            check(cuda.cuMemcpyHtoD_v2(device_input, host_input, len(raw_input)))
            args = (c.c_void_p * 2)(c.addressof(device_input), c.addressof(device_output))
            started = time.monotonic()
            check(cuda.cuLaunchKernel(kernel, 1, 1, 1, 32, 1, 1, 0, None, args, None))
            check(cuda.cuCtxSynchronize())
            kernel_seconds += time.monotonic() - started
            host_output = c.create_string_buffer(OUTPUT_BYTES)
            check(cuda.cuMemcpyDtoH_v2(host_output, device_output, OUTPUT_BYTES))
            raw_output = host_output.raw
            input_digest = hashlib.sha256(raw_input).digest()
            output_digest = hashlib.sha256(raw_output).digest()
            score = ticket(challenge, nonce, input_digest, output_digest)
            attempts += 1
            stop = False
            if on_candidate is not None:
                # Trusted in-process pool adapter; submit ordinary shares as well
                # as chain-target proofs. The standalone CLI is unchanged.
                stop = on_candidate(nonce, raw_input, raw_output, score)
            if score <= target:
                winner = (nonce, raw_input, raw_output, input_digest, output_digest, score)
                break
            if stop:
                break
    finally:
        for label, handle, release in (
            ("output", device_output, lambda: cuda.cuMemFree_v2(device_output)),
            ("input", device_input, lambda: cuda.cuMemFree_v2(device_input)),
            ("module", module, lambda: cuda.cuModuleUnload(module)),
            ("program", program, lambda: nvrtc.nvrtcDestroyProgram(c.byref(program))),
            ("context", context, lambda: cuda.cuCtxDestroy_v2(context)),
        ):
            if handle.value:
                try:
                    check(release())
                except BaseException as error:
                    cleanup_errors.append(f"{label}: {error}")
        if cleanup_errors:
            raise RuntimeError("cleanup uncertain: " + "; ".join(cleanup_errors))

    common = {
        "schema": "EAGW-POW-V2-GPU-SEARCH",
        "challenge": challenge.hex(),
        "target": target.hex(),
        "startNonce": start_nonce,
        "attempts": attempts,
        "maxAttempts": max_attempts,
        "gpuUuidHex": bytes(uuid).hex(),
        "gpuUuid": actual_gpu_uuid,
        "kernelSha256": hashlib.sha256(KERNEL).hexdigest(),
        "kernelSecondsTotal": kernel_seconds,
        "physicalGpuAttested": False,
        "scope": "real bounded GPU arithmetic with CPU-verifiable V2 ticket; not full consensus",
    }
    if winner is None:
        common["status"] = "target-not-found"
        save(out / "result.json", json.dumps(common, sort_keys=True, indent=2).encode())
        return
    nonce, raw_input, raw_output, input_digest, output_digest, score = winner
    certificate = (
        bytes([VERSION])
        + challenge
        + struct.pack("<Q", nonce)
        + input_digest
        + output_digest
    )
    save(out / "input.bin", raw_input)
    save(out / "output.bin", raw_output)
    save(out / "certificate.bin", certificate)
    common.update(
        {
            "status": "found",
            "nonce": nonce,
            "inputSha256": input_digest.hex(),
            "outputSha256": output_digest.hex(),
            "ticket": score.hex(),
            "certificateSha256": hashlib.sha256(certificate).hexdigest(),
        }
    )
    save(out / "result.json", json.dumps(common, sort_keys=True, indent=2).encode())
    del builtins


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("challenge")
    parser.add_argument("target")
    parser.add_argument("output")
    parser.add_argument("--start-nonce", type=int, default=0)
    parser.add_argument("--max-attempts", type=int, default=64)
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--gpu-uuid", required=True)
    args = parser.parse_args()
    if len(args.challenge) != 64 or any(x not in "0123456789abcdef" for x in args.challenge):
        parser.error("challenge must be 32-byte lowercase hex")
    if len(args.target) != 64 or any(x not in "0123456789abcdef" for x in args.target):
        parser.error("target must be 32-byte lowercase big-endian hex")
    if not 0 <= args.start_nonce <= 0xFFFFFFFFFFFFFFFF:
        parser.error("start nonce must fit u64")
    if not 1 <= args.max_attempts <= 1024:
        parser.error("max attempts must be 1..1024")
    if not 1 <= args.timeout <= 60:
        parser.error("timeout must be 1..60 seconds")
    if not valid_canonical_gpu_uuid(args.gpu_uuid):
        parser.error("gpu uuid must be canonical GPU-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")
    output = Path(args.output).absolute()
    output.mkdir(mode=0o700)
    challenge = bytes.fromhex(args.challenge)
    target = bytes.fromhex(args.target)
    save(
        output / "intent.json",
        json.dumps(
            {
                "schema": "EAGW-POW-V2-INTENT",
                "challenge": args.challenge,
                "target": args.target,
                "startNonce": args.start_nonce,
                "maxAttempts": args.max_attempts,
                "status": "started",
                "retry": "forbidden",
            },
            sort_keys=True,
        ).encode(),
    )
    try:
        context = multiprocessing.get_context("spawn")
        process = context.Process(
            target=worker,
            args=(
                challenge,
                target,
                args.start_nonce,
                args.max_attempts,
                output,
                time.monotonic() + args.timeout,
                args.gpu_uuid,
            ),
        )
        process.start()
        process.join(args.timeout)
        if process.is_alive():
            process.terminate()
            process.join(5)
            if process.is_alive():
                process.kill()
                process.join(5)
            raise RuntimeError(
                f"timeout; execution and cleanup unknown; worker alive={process.is_alive()}"
            )
        if process.exitcode:
            raise RuntimeError(f"worker exited {process.exitcode}; execution state unknown")
    except BaseException as error:
        save(
            output / "failure.json",
            json.dumps(
                {"status": "unknown", "error": str(error), "retry": "forbidden"},
                sort_keys=True,
            ).encode(),
        )
        raise


if __name__ == "__main__":
    main()
