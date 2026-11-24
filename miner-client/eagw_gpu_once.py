"""One bounded local EAGW calculation; output is not GPU attestation."""
import argparse
import ctypes as c
import hashlib
import json
import os
from pathlib import Path
import struct
import multiprocessing
import sys
import time

INPUT_DOMAIN = b'EAGW-INPUT-V1\0'
COMMIT_DOMAIN = b'EAGW-COMMIT-V1\0'
CUDA_PATH = Path('/usr/lib/wsl/lib/libcuda.so.1')
NVRTC_PATH = Path('/usr/lib/x86_64-linux-gnu/libnvrtc.so.12.0.140')
NVRTC_BUILTINS_PATH = Path('/usr/lib/x86_64-linux-gnu/libnvrtc-builtins.so.12.0.140')
LIBRARY_SHA256 = {
    CUDA_PATH: 'ebf80473fc72cd56aa804eacbe00d1de4bf51b0552f412c4f405790c0f5bb238',
    NVRTC_PATH: 'd7c32a86aa8321ef60a911b7b0c6655db489cae21481c71e166a684cbd54c62a',
    NVRTC_BUILTINS_PATH: '8cb4db2d2806cd3b85e60023f5988811584d7f0fef51d1a4d380faf4e6432c74',
}
KERNEL = b'''extern "C" __global__ void matrix(const int* in, int* out) {
 int i=threadIdx.x; if(i<32) { int row=i/16, col=i%16, sum=0;
 for(int k=0;k<2048;k++) sum += in[row*2048+k]*in[(2+col)*2048+k];
 out[i]=sum; }}'''

def input_bytes(subject):
    stream = b''.join(hashlib.blake2s(INPUT_DOMAIN + subject + struct.pack('<I', i)).digest()
                      for i in range((18*2048+31)//32))[:18*2048]
    return b''.join(struct.pack('<i', x % 129 - 64) for x in stream)

def save(path, data):
    with open(path, 'xb') as f:
        f.write(data); f.flush(); os.fsync(f.fileno())

def check(status):
    if status: raise RuntimeError('CUDA/NVRTC status %d' % status)

def verify_runtime_libraries():
    for path, expected in LIBRARY_SHA256.items():
        if not path.is_file() or path.is_symlink():
            raise RuntimeError('pinned regular runtime library required: ' + str(path))
        digest = hashlib.sha256()
        with open(path, 'rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
        if digest.hexdigest() != expected:
            raise RuntimeError('runtime library pin mismatch: ' + str(path))

def worker(subject, out):
    verify_runtime_libraries()
    builtins = c.CDLL(str(NVRTC_BUILTINS_PATH), mode=os.RTLD_NOW | os.RTLD_GLOBAL)
    cuda = c.CDLL(str(CUDA_PATH), mode=os.RTLD_NOW | os.RTLD_LOCAL)
    nv = c.CDLL(str(NVRTC_PATH), mode=os.RTLD_NOW | os.RTLD_LOCAL)
    sig = {'cuInit':[c.c_uint], 'cuDeviceGet':[c.POINTER(c.c_int),c.c_int],
      'cuDeviceGetUuid':[c.c_void_p,c.c_int], 'cuDeviceComputeCapability':[c.POINTER(c.c_int),c.POINTER(c.c_int),c.c_int],
      'cuCtxCreate_v2':[c.POINTER(c.c_void_p),c.c_uint,c.c_int], 'cuCtxDestroy_v2':[c.c_void_p],
      'cuModuleLoadData':[c.POINTER(c.c_void_p),c.c_void_p], 'cuModuleUnload':[c.c_void_p],
      'cuModuleGetFunction':[c.POINTER(c.c_void_p),c.c_void_p,c.c_char_p],
      'cuMemAlloc_v2':[c.POINTER(c.c_uint64),c.c_size_t], 'cuMemFree_v2':[c.c_uint64],
      'cuMemcpyHtoD_v2':[c.c_uint64,c.c_void_p,c.c_size_t], 'cuMemcpyDtoH_v2':[c.c_void_p,c.c_uint64,c.c_size_t],
      'cuLaunchKernel':[c.c_void_p]+[c.c_uint]*7+[c.c_void_p,c.POINTER(c.c_void_p),c.c_void_p], 'cuCtxSynchronize':[]}
    for name, args in sig.items():
        fn=getattr(cuda,name); fn.argtypes=args; fn.restype=c.c_int
    ns={'nvrtcCreateProgram':[c.POINTER(c.c_void_p),c.c_char_p,c.c_char_p,c.c_int,c.c_void_p,c.c_void_p],
        'nvrtcCompileProgram':[c.c_void_p,c.c_int,c.POINTER(c.c_char_p)],
        'nvrtcGetPTXSize':[c.c_void_p,c.POINTER(c.c_size_t)], 'nvrtcGetPTX':[c.c_void_p,c.c_void_p],
        'nvrtcDestroyProgram':[c.POINTER(c.c_void_p)]}
    for name,args in ns.items():
        fn=getattr(nv,name); fn.argtypes=args; fn.restype=c.c_int
    ctx=c.c_void_p(); mod=c.c_void_p(); prog=c.c_void_p(); a=c.c_uint64(); b=c.c_uint64()
    errors=[]; output=None
    try:
        check(cuda.cuInit(0)); dev=c.c_int(); check(cuda.cuDeviceGet(c.byref(dev),0))
        major=c.c_int(); minor=c.c_int(); check(cuda.cuDeviceComputeCapability(c.byref(major),c.byref(minor),dev))
        if (major.value,minor.value)!=(8,6): raise RuntimeError('device must be compute capability 8.6')
        uuid=(c.c_ubyte*16)(); check(cuda.cuDeviceGetUuid(uuid,dev))
        check(cuda.cuCtxCreate_v2(c.byref(ctx),0,dev))
        check(nv.nvrtcCreateProgram(c.byref(prog),KERNEL,b'eagw.cu',0,None,None))
        opts=(c.c_char_p*1)(b'--gpu-architecture=compute_86'); check(nv.nvrtcCompileProgram(prog,1,opts))
        n=c.c_size_t(); check(nv.nvrtcGetPTXSize(prog,c.byref(n)))
        if not 0<n.value<=16*1024*1024: raise RuntimeError('invalid PTX size')
        ptx=c.create_string_buffer(n.value); check(nv.nvrtcGetPTX(prog,ptx))
        check(cuda.cuModuleLoadData(c.byref(mod),ptx)); fn=c.c_void_p(); check(cuda.cuModuleGetFunction(c.byref(fn),mod,b'matrix'))
        raw=input_bytes(subject); save(out/'input.bin',raw)
        check(cuda.cuMemAlloc_v2(c.byref(a),len(raw))); check(cuda.cuMemAlloc_v2(c.byref(b),128))
        inp=c.create_string_buffer(raw); check(cuda.cuMemcpyHtoD_v2(a,inp,len(raw)))
        args=(c.c_void_p*2)(c.addressof(a),c.addressof(b)); started=time.monotonic()
        check(cuda.cuLaunchKernel(fn,1,1,1,32,1,1,0,None,args,None)); check(cuda.cuCtxSynchronize())
        elapsed=time.monotonic()-started; result=c.create_string_buffer(128); check(cuda.cuMemcpyDtoH_v2(result,b,128)); output=result.raw
    finally:
        for label, call in [('output',lambda:cuda.cuMemFree_v2(b)),('input',lambda:cuda.cuMemFree_v2(a)),
                            ('module',lambda:cuda.cuModuleUnload(mod)),('program',lambda:nv.nvrtcDestroyProgram(c.byref(prog))),
                            ('context',lambda:cuda.cuCtxDestroy_v2(ctx))]:
            handle={'output':b,'input':a,'module':mod,'program':prog,'context':ctx}[label]
            if handle.value:
                try: check(call())
                except BaseException as e: errors.append(label+': '+str(e))
        if errors: raise RuntimeError('cleanup uncertain: '+ '; '.join(errors))
    save(out/'output.bin',output)
    ih=hashlib.sha256(raw).digest(); oh=hashlib.sha256(output).digest(); commitment=hashlib.sha256(COMMIT_DOMAIN+subject+ih+oh).hexdigest()
    info={'schema':'EAGW-GPU-ONCE-V1','subject':subject.hex(),'gpuUuidHex':bytes(uuid).hex(),
          'kernelSha256':hashlib.sha256(KERNEL).hexdigest(),'inputSha256':ih.hex(),
          'outputSha256':hashlib.sha256(output).hexdigest(),'commitment':commitment,
          'inputDefinition':'BLAKE2s-256(EAGW-INPUT-V1\\0 || subject || counter_u32_le), counter starts 0; bytes % 129 - 64; 18x2048 i32 little-endian; A rows 0..1, B rows 2..17',
          'commitmentDefinition':'SHA256(EAGW-COMMIT-V1\\0 || subject || inputSha256 raw bytes || outputSha256 raw bytes)',
          'grid':[1,1,1],'block':[32,1,1],'kernelLaunches':1,'kernelSeconds':elapsed,
          'gpuExecutionAttested':False,'scope':'recomputable GPU-assisted work; no physical GPU proof'}
    del builtins
    save(out/'result.json',json.dumps(info,sort_keys=True,indent=2).encode())

def main():
    p=argparse.ArgumentParser(); p.add_argument('subject'); p.add_argument('output'); p.add_argument('--timeout',type=int,default=30)
    args=p.parse_args()
    if len(args.subject)!=64 or any(x not in '0123456789abcdef' for x in args.subject): p.error('subject must be 32-byte lowercase hex')
    if not 1<=args.timeout<=60: p.error('timeout must be 1..60 seconds')
    out=Path(args.output).absolute(); subject=bytes.fromhex(args.subject)
    out.mkdir(mode=0o700)
    save(out/'intent.json',json.dumps({'subject':args.subject,'status':'started','retry':'forbidden'}).encode())
    try:
        process=multiprocessing.get_context('spawn').Process(target=worker,args=(subject,out))
        process.start(); process.join(args.timeout)
        if process.is_alive():
            process.terminate(); process.join(5)
            if process.is_alive(): process.kill(); process.join(5)
            raise RuntimeError('timeout; execution and cleanup unknown; worker alive=%s' % process.is_alive())
        if process.exitcode: raise RuntimeError('worker exited %s; execution state unknown' % process.exitcode)
    except BaseException as e:
        save(out/'failure.json',json.dumps({'status':'unknown','error':str(e),'retry':'forbidden'}).encode()); raise

if __name__=='__main__': main()
