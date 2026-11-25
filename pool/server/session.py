"""One SMW/1 session on a connected TLS socket supplied by systemd."""
import hashlib
import json
import os
import re
import secrets
import socket
import sqlite3
import ssl
import sys
import time
from pathlib import Path
from share_backend import receipt, template, TARGET
from smw_share import MX10_WORK_ALGORITHM

ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

def identity(address, name):
    if not isinstance(address, str) or not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,48}", name):
        raise ValueError("invalid login")
    address = address.removeprefix("SVY")
    if len(address)!=48 or not address.startswith("5"):
        raise ValueError("invalid payout address")
    number = 0
    for c in address:
        number = number * 58 + ALPHABET.index(c)
    raw=number.to_bytes((number.bit_length()+7)//8,"big")
    if len(raw)!=35 or raw[0]!=42 or hashlib.blake2b(b"SS58PRE"+raw[:-2]).digest()[:2]!=raw[-2:]:
        raise ValueError("invalid address checksum")
    return address+"."+name,"SVY"+address

def msg(reader):
    line=reader.readline(8193)
    if not line:return None
    if len(line)>8192 or not line.endswith(b"\n"):raise ValueError("invalid request size")
    data=json.loads(line)
    if not isinstance(data,dict):raise ValueError("invalid object")
    return data

def serve(reader, writer):
    login=msg(reader)
    if not login or login.get("protocol")!="SMW/1":raise ValueError("invalid protocol")
    worker,wallet=identity(login["wallet"],login["worker"])
    job=None
    start=0
    request=login
    with sqlite3.connect(Path(os.environ.get("SVY_SHARE_DB","/var/lib/svy-pool-shares/shares.sqlite")).resolve().as_uri()+"?mode=rw",uri=True,timeout=3) as db:
        while request is not None:
            proofs=request.get("proofs",[])
            if not isinstance(proofs,list) or len(proofs)>32 or any(not isinstance(p,str) or not re.fullmatch("[0-9a-f]{210}",p) for p in proofs):
                raise ValueError("invalid proofs")
            if proofs and (not job or request.get("preHash")!=job["preHash"]):
                raise ValueError("unassigned job")
            results=[receipt(db,job,start,p,worker,wallet) for p in proofs]
            current=template()
            if current["workAlgorithm"]!=MX10_WORK_ALGORITHM:raise ValueError("unsupported mainnet job")
            if current!=job:
                job=current
                start=(1<<62)+secrets.randbits(60)
            writer.write((json.dumps(dict(protocol="SMW/1",job=job,start=start,wallet=wallet,worker=worker,target=f"{TARGET:064x}",receipts=results))+"\n").encode())
            writer.flush()
            time.sleep(0.05)
            request=msg(reader)

if __name__=="__main__":
    serve(sys.stdin.buffer,sys.stdout.buffer)
