"""Local CUDA + TLS acceptance test; uses no network outside loopback."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socketserver
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import threading
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "server"))
parser = argparse.ArgumentParser()
parser.add_argument("rig", type=Path, help="unpacked Linux multi-GPU client directory")
parser.add_argument("--max-watts", type=int, required=True)
args = parser.parse_args()
with tempfile.TemporaryDirectory() as name:
    tmp = Path(name)
    for file in ("smw-cuda", "smw_stream.py", "smw_share.py"):
        shutil.copy2(args.rig / file, tmp / file)
    (tmp / "smw-cuda").chmod(0o700)
    for file in ("miner.py", "pool_endpoint.py"):
        shutil.copy2(root / "client" / file, tmp / file)
    db = tmp / "shares.sqlite"
    os.environ.update(SVY_SHARE_DB=str(db), SVY_POOL_JOB=str(tmp / "job.json"))
    import session
    import share_backend
    subprocess.run([sys.executable, root / "server/init_db.py"], check=True)
    subprocess.run(["openssl","req","-x509","-newkey","rsa:2048","-nodes","-days","1",
                    "-subj","/CN=localhost","-addext","subjectAltName=DNS:localhost",
                    "-keyout",str(tmp/"key"),"-out",str(tmp/"cert")],check=True,stderr=subprocess.DEVNULL)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(tmp/"cert",tmp/"key")
    # Only finalized height and the job are fixtures; the GPU and proof verifier are real.
    share_backend.rpc = lambda method, params: {"number":"0x1"} if method=="chain_getHeader" else "0x"+"01"*32
    (tmp/"job.json").write_text(json.dumps(dict(height=2,parentHash="0x"+"01"*32,preHash="0x"+"02"*32,
        genesis="0x"+share_backend.MAINNET_GENESIS, workAlgorithm=session.MX10_WORK_ALGORITHM,chainTarget="0x"+"00"*32)))
    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            try:
                with tls.wrap_socket(self.request,server_side=True) as sock, sock.makefile("rb") as r, sock.makefile("wb") as w:
                    session.serve(r,w)
            except (BrokenPipeError, ConnectionResetError):
                pass
    with socketserver.ThreadingTCPServer(("127.0.0.1",0),Handler) as server:
        server.daemon_threads=True
        threading.Thread(target=server.serve_forever,daemon=True).start()
        env=dict(os.environ,SSL_CERT_FILE=str(tmp/"cert"),SMW_POOL_URL=f"tls://localhost:{server.server_address[1]}")
        # A public fixture account is sufficient; no chain transaction is constructed.
        worker=subprocess.Popen([sys.executable,"-Bu",str(tmp/"miner.py"),"--worker","smoke",
            "--wallet","SVY5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY","--max-watts",str(args.max_watts)],
            env=env,start_new_session=True)
        try:
            deadline=time.monotonic()+18
            with sqlite3.connect(db) as conn:
                while time.monotonic()<deadline:
                    count=conn.execute("SELECT count(*) FROM gpu_stream_shares").fetchone()[0]
                    if count:
                        print(f"Verified GPU shares over independent TLS endpoint: {count}")
                        break
                    time.sleep(.5)
                else: raise RuntimeError("No verified GPU share before timeout")
        finally:
            os.killpg(worker.pid, signal.SIGTERM)
            worker.wait(timeout=5)
            server.shutdown()
