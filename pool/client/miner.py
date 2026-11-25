import argparse
import csv
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import socket
import ssl
import subprocess
import time

from smw_share import MX10_WORK_ALGORITHM, challenge_for
from smw_stream import SmwStream

ROOT = Path(__file__).resolve().parent
from pool_endpoint import endpoint
HOST, PORT, POOL_CERT = endpoint(ROOT)
GENESIS = "0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8"


def sensors(gpu, cap, temp_limit):
    smi = shutil.which("nvidia-smi") or "/usr/lib/wsl/lib/nvidia-smi"
    result = subprocess.check_output([smi, "-i", gpu,
        "--query-gpu=uuid,temperature.gpu,power.limit", "--format=csv,noheader,nounits"],
        text=True, timeout=3).strip()
    uuid, temp, limit = [s.strip() for s in next(csv.reader([result]))]
    if not re.fullmatch(r"GPU-[a-fA-F0-9-]+", uuid) or not 0 < float(temp) < temp_limit:
        raise RuntimeError("GPU temperature or identity outside configured limits")
    if cap and not 0 < float(limit) <= cap:
        raise RuntimeError("Set the GPU power limit before mining")
    return uuid, float(temp)


def run(args, uuid):
    ctx = ssl.create_default_context(cafile=POOL_CERT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    engine = None
    try:
        with socket.create_connection((HOST,PORT), timeout=4) as tcp, ctx.wrap_socket(tcp, server_hostname=HOST) as sock, sock.makefile("rb") as reader:
            sock.settimeout(3)
            request = dict(protocol="SMW/1", worker=args.worker, wallet=args.wallet)
            key = None
            found = []
            accepted = attempts = 0
            report = last_guard = time.monotonic()
            temp = None
            while True:
                sock.sendall((json.dumps(request)+"\n").encode())
                line = reader.readline(8193)
                if not line.endswith(b"\n") or len(line)>8192:
                    raise ValueError("Incomplete pool response")
                data = json.loads(line)
                job = data["job"]
                if (data["protocol"]!="SMW/1" or data["wallet"]!=args.wallet or
                    data["worker"]!=args.wallet[3:]+"."+args.worker or
                    job["genesis"]!=GENESIS or job["workAlgorithm"]!=MX10_WORK_ALGORITHM):
                    raise ValueError("Mainnet assignment mismatch")
                receipts = data["receipts"]
                if len(receipts)!=len(request.get("proofs",[])) or any(x not in ("accepted","late","duplicate") for x in receipts):
                    raise ValueError("Invalid pool receipts")
                accepted += receipts.count("accepted")
                args.rig_accepted += receipts.count("accepted")
                args.rig_rejected += receipts.count("late")+receipts.count("duplicate")
                new_key = (job["preHash"],data["start"],data["target"])
                if new_key != key:
                    if engine:
                        engine.close()
                        engine = None
                    _,temp = sensors(uuid,args.max_watts,args.max_temp)
                    challenge = bytes.fromhex(challenge_for(GENESIS,job["preHash"][2:]))
                    engine = SmwStream(str(ROOT/"smw-cuda"),challenge,data["start"],bytes.fromhex(data["target"]),uuid)
                    found.clear()
                    key = new_key
                until = time.monotonic()+0.25
                while time.monotonic()<until and len(found)<32:
                    proofs,count = engine.poll()
                    found.extend(proofs)
                    attempts += count
                request = dict(preHash=key[0],proofs=found[:32])
                del found[:32]
                now = time.monotonic()
                if now-last_guard>=1:
                    _,temp = sensors(uuid,args.max_watts,args.max_temp)
                    last_guard = now
                if now-report>=5:
                    stats=dict(worker=args.worker,height=job["height"],hps=round(attempts/(now-report)),accepted=accepted,temp=temp,rigAccepted=args.rig_accepted,rigRejected=args.rig_rejected,updated=int(time.time()))
                    print(json.dumps(stats),flush=True)
                    if args.stats_file:
                        dest=Path(args.stats_file)
                        tmp=dest.with_name(dest.name+".tmp")
                        tmp.write_text(json.dumps(stats))
                        os.replace(tmp,dest)
                    report=now
                    attempts=0
    finally:
        if engine:
            engine.close()


def main():
    p=argparse.ArgumentParser(description="Solvyren SMW-16/2048 v1 mainnet miner")
    p.add_argument("--wallet",required=True)
    p.add_argument("--worker",required=True)
    p.add_argument("--gpu",default="0")
    p.add_argument("--max-watts",type=int)
    p.add_argument("--max-temp",type=int,default=80)
    p.add_argument("--stats-file")
    args=p.parse_args()
    args.rig_accepted=args.rig_rejected=0
    if not args.wallet.startswith("SVY"):
        args.wallet="SVY"+args.wallet
    if not re.fullmatch(r"SVY5[1-9A-HJ-NP-Za-km-z]{47}",args.wallet) or not re.fullmatch(r"[A-Za-z0-9_-]{1,48}",args.worker):
        p.error("Invalid wallet or worker")
    while True:
        try:
            uuid,_=sensors(args.gpu,args.max_watts,args.max_temp)
            owner=Path("/var/lib/eagle-ai/gpu-pow")/uuid
            path=owner.with_suffix(".lock")
            if not path.exists():
                path=Path(os.environ.get("XDG_RUNTIME_DIR",str(Path.home())))/(".solvyren-"+uuid+".lock")
            with path.open("r" if path.exists() else "a") as lock:
                fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
                if any(os.path.lexists(owner/("eagw-pow-v2-global-"+s)) for s in ("active","unknown","transition")):
                    raise RuntimeError("GPU reserved")
                run(args,uuid)
        except (ValueError,KeyError,RuntimeError,OSError,subprocess.SubprocessError) as error:
            print("Retrying in 3 seconds:",error,flush=True)
            time.sleep(3)


if __name__=="__main__":
    main()
