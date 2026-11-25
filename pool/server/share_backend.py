"""Private authenticated GPU session; every credited ticket is recomputed locally."""
import json
import os
import secrets
import tempfile
import sqlite3
import sys
import time
from pathlib import Path
from urllib.request import Request, urlopen

MAINNET_GENESIS = "678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8"
UINT256_MAX = (1<<256)-1
import smw_share


DIFFICULTY = 65536
TARGET = UINT256_MAX // DIFFICULTY
ROOT = Path(os.environ.get("SVY_POOL_JOB","/run/svy-pool/job.json"))


def rpc(method, params):
    req = Request(os.environ.get("SVY_POOL_RPC","http://127.0.0.1:9954"), json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=params)).encode(),
                  {"Content-Type": "application/json"})
    with urlopen(req, timeout=3) as response:
        return json.load(response)["result"]


def template():
    job = json.loads(ROOT.read_text())
    if job["genesis"] != "0x"+MAINNET_GENESIS or job["workAlgorithm"] != smw_share.MX10_WORK_ALGORITHM:
        raise ValueError("invalid template")
    if not smw_share.hex32(job["parentHash"][2:]): raise ValueError("invalid parent")
    if not 0 <= time.time()-ROOT.stat().st_mtime <= 20:
        best = rpc("chain_getBlockHash", [])
        if best != job["parentHash"] or int(rpc("chain_getHeader", [best])["number"], 16) + 1 != job["height"]:
            raise ValueError("stale template")
    return job


def publish_winning_proof(job, raw):
    # The author independently verifies against its current template and target.
    if template() != job:
        return
    with tempfile.NamedTemporaryFile(dir=ROOT.parent, prefix="smw-seal-") as candidate:
        os.fchmod(candidate.fileno(), 0o640)
        candidate.write(raw)
        candidate.flush()
        try:
            os.link(candidate.name, ROOT.parent/"seal.bin")
        except FileExistsError:
            pass


def receipt(db, job, start, cert, worker, wallet):
    raw = bytes.fromhex(cert)
    challenge = smw_share.challenge_for(job["genesis"], job["preHash"][2:])
    if len(raw)!=105 or raw[0]!=10 or raw[1:33].hex()!=challenge:
        raise ValueError("invalid certificate")
    nonce = int.from_bytes(raw[33:41], "little")
    if not start <= nonce < start + 2**32:
        raise ValueError("nonce outside lease")
    fields = dict(challenge=raw[1:33], nonce=nonce, input_digest=raw[41:73], output_digest=raw[73:105])
    if smw_share.claimed_ticket(fields)>TARGET: raise ValueError("above share target")
    expected, score = smw_share.compute_certificate(challenge, nonce)
    if cert != expected or int(score,16)>TARGET: raise ValueError("invalid GPU work")
    with db:
        db.execute("BEGIN IMMEDIATE")
        if db.execute("SELECT height FROM watermark WHERE id=1").fetchone()[0] >= job["height"]:
            return "late"
        final = rpc("chain_getHeader",[rpc("chain_getFinalizedHead",[])])
        if int(final["number"],16)>=job["height"]:
            return "late"
        cur = db.execute("INSERT OR IGNORE INTO gpu_stream_shares VALUES(?,?,?,?,?,?,?)",
                         (challenge,str(nonce),job["height"],job["parentHash"],worker,wallet,DIFFICULTY))
        accepted = bool(cur.rowcount)
    if accepted and int(score,16)<=int(job["chainTarget"],16):
        publish_winning_proof(job,raw)
    return "accepted" if accepted else "duplicate"
