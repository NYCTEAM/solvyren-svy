"""Windows OpenCL client for the Solvyren mainnet pool."""
import argparse
import os
import hashlib
import json
from pathlib import Path
import re
import socket
import ssl
import sys
import time

import numpy as np
import pyopencl as cl
import smw_share as smw

ROOT = Path(__file__).resolve().parent
from pool_endpoint import endpoint
HOST, PORT, POOL_CERT = endpoint(ROOT)
GENESIS = "0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8"
ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def wallet_address(text):
    raw = text.strip()
    if raw.startswith("SVY"):
        raw = raw[3:]
    if not re.fullmatch(r"5[1-9A-HJ-NP-Za-km-z]{47}", raw):
        raise ValueError("Enter a Solvyren mainnet public wallet address.")
    n = 0
    for ch in raw:
        n = n * 58 + ALPHABET.index(ch)
    buf = n.to_bytes(35, "big")
    if buf[0] != 42 or buf[-2:] != hashlib.blake2b(b"SS58PRE"+buf[:-2]).digest()[:2]:
        raise ValueError("Wallet checksum mismatch.")
    return "SVY"+raw


class Engine:
    def __init__(self, device, batch):
        if not device.endian_little:
            raise ValueError("This miner requires a little-endian OpenCL device.")
        self.ctx = cl.Context([device])
        self.queue = cl.CommandQueue(self.ctx)
        program = cl.Program(self.ctx, (ROOT/"smw.cl").read_text()).build(options=["-cl-std=CL1.2"])
        self.derive = program.k_derive
        self.matmul = program.k_matmul
        self.digests = program.k_digests
        self.batch = batch
        def buffer(size):
            return cl.Buffer(self.ctx, cl.mem_flags.READ_WRITE, size)
        self.challenge = buffer(32)
        self.values = buffer(batch*65536)
        self.products = buffer(batch*1024)
        self.dig = buffer(batch*64)

    def assign(self, challenge):
        cl.enqueue_copy(self.queue, self.challenge, challenge, is_blocking=True)

    def compute(self, nonce, count):
        n = np.uint32(16)
        size = np.uint32(count)
        self.derive(self.queue,(2048*count,),None,self.challenge,self.values,np.uint64(nonce),n,size)
        self.matmul(self.queue,(256*count,),None,self.values,self.products)
        self.digests(self.queue,(count,),None,self.values,self.products,self.dig,n,size)
        out=np.empty((count,64),np.uint8)
        cl.enqueue_copy(self.queue,out,self.dig,is_blocking=True)
        return out

    def selftest(self):
        for challenge,base in [(bytes(range(32)),(1<<62)+41),(bytes(32),0)]:
            self.assign(challenge)
            for i,dig in enumerate(self.compute(base,2)):
                cert,_=smw.compute_certificate(challenge.hex(),base+i)
                if dig.tobytes()!=bytes.fromhex(cert)[41:]:
                    raise RuntimeError("GPU self-test failed. Check the driver and GPU settings.")
        print("SMW-16/2048 v1 self-test passed (4 full proofs).", flush=True)


def session(engine,args,report_stats=None):
    ctx=ssl.create_default_context(cafile=POOL_CERT)
    with socket.create_connection((HOST,PORT),timeout=8) as conn, ctx.wrap_socket(conn,server_hostname=HOST) as sock, sock.makefile("rb") as reader:
        sock.settimeout(8)
        request=dict(protocol="SMW/1",wallet=args.wallet,worker=args.worker)
        key=None
        attempts=accepted=refused=0
        report=time.monotonic()
        while True:
            sock.sendall(json.dumps(request).encode()+b"\n")
            line=reader.readline(8193)
            if not line or not line.endswith(b"\n") or len(line)>8192:
                raise ConnectionError("Incomplete pool response")
            reply=json.loads(line)
            job=reply["job"]
            if (reply["protocol"]!="SMW/1" or reply["wallet"]!=args.wallet or
                reply["worker"]!=args.wallet[3:]+"."+args.worker or job["genesis"]!=GENESIS or
                job["workAlgorithm"]!=smw.MX10_WORK_ALGORITHM):
                raise ValueError("Unexpected mainnet assignment")
            receipts=reply["receipts"]
            if len(receipts)!=len(request.get("proofs",[])) or any(x not in ("accepted","late","duplicate") for x in receipts):
                raise ValueError("Invalid pool receipts")
            accepted+=receipts.count("accepted")
            refused+=receipts.count("late")+receipts.count("duplicate")
            new=(job["preHash"],reply["start"],reply["target"])
            if new!=key:
                challenge=bytes.fromhex(smw.challenge_for(GENESIS,new[0][2:]))
                engine.assign(challenge)
                base=nonce=new[1]
                target=int(new[2],16)
                key=new
            proofs=[]
            until=time.monotonic()+0.45
            while time.monotonic()<until and len(proofs)<32:
                # Even with an unusually easy target one request stays within 32 receipts.
                count=min(engine.batch,base+(1<<32)-nonce)
                if count<=0:
                    raise ConnectionError("Renewing nonce lease")
                started=time.monotonic()
                batch=engine.compute(nonce,count)
                processed=0
                for dig in batch:
                    value=dig.tobytes()
                    fields=dict(challenge=challenge,nonce=nonce,input_digest=value[:32],output_digest=value[32:])
                    if smw.claimed_ticket(fields)<=target:
                        proofs.append((b"\x0a"+challenge+nonce.to_bytes(8,"little")+value).hex())
                    nonce+=1
                    processed+=1
                    if len(proofs)==32:
                        break
                attempts+=processed
                time.sleep((time.monotonic()-started)*(100/args.duty-1))
            request=dict(preHash=job["preHash"],proofs=proofs)
            now=time.monotonic()
            if now-report>=5:
                if report_stats:
                    report_stats(job["height"],round(attempts/(now-report)),accepted,refused)
                print(f"Block #{job['height']}  {attempts/(now-report):,.0f} H/s  accepted={accepted}",flush=True)
                attempts=0
                report=now


def main():
    p=argparse.ArgumentParser()
    p.add_argument("--wallet")
    p.add_argument("--worker")
    p.add_argument("--gpu",type=int,default=0)
    p.add_argument("--list",action="store_true")
    p.add_argument("--self-test",action="store_true")
    p.add_argument("--diagnostic-any-vendor",action="store_true",help="Allow a non-AMD GPU for offline self-test only")
    p.add_argument("--batch",type=int,choices=[16,32,64,128,256],default=64)
    p.add_argument("--duty",type=int,choices=range(10,101),metavar="10..100",default=65)
    args=p.parse_args()
    if args.diagnostic_any_vendor and not args.self_test:
        p.error("Use --self-test with diagnostic mode")
    devices=[d for platform in cl.get_platforms() for d in platform.get_devices(device_type=cl.device_type.GPU)
             if d.vendor_id==0x1002 or args.diagnostic_any_vendor]
    if not devices:
        raise RuntimeError("No AMD OpenCL GPU found. Install the AMD graphics driver for your GPU and restart this miner.")
    for i,d in enumerate(devices):
        print(f"[{i}] {d.name.strip()} | {d.vendor} | driver {d.driver_version}",flush=True)
    if args.list:
        return
    if not 0<=args.gpu<len(devices):
        p.error("Invalid GPU number; use --list")
    if not args.self_test:
        args.wallet=wallet_address(args.wallet or input("SVY mainnet payout address: "))
        args.worker=(args.worker or input("Worker name [amd-pc]: ").strip() or "amd-pc")
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,48}",args.worker):
            raise ValueError("Use 1-48 letters, digits, underscores or hyphens for the worker name.")
    engine=Engine(devices[args.gpu],args.batch)
    engine.selftest()
    if args.self_test:
        return
    print(f"Mainnet {HOST}:{PORT} | {args.wallet} | {args.worker} | duty {args.duty}% | Ctrl+C to stop",flush=True)
    while True:
        try:
            session(engine,args)
        except (ConnectionError,TimeoutError,OSError) as error:
            if isinstance(error,ssl.SSLCertVerificationError):
                raise
            print(f"Reconnecting in 3 seconds: {error}",flush=True)
            time.sleep(3)


if __name__=="__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
    except Exception as error:
        print("Miner stopped:",error,file=sys.stderr,flush=True)
        sys.exit(1)
