"""Seal finalized rounds after all earlier admitted share writes have committed."""
import os
import sqlite3
import time
from share_backend import MAINNET_GENESIS, rpc

if __name__ == "__main__":
    if rpc("chain_getBlockHash", [0]) != "0x" + MAINNET_GENESIS:
        raise RuntimeError("Unexpected chain genesis")
    with sqlite3.connect(os.environ.get("SVY_SHARE_DB", "/var/lib/svy-pool-shares/shares.sqlite"),timeout=10) as db:
        while True:
            final = rpc("chain_getHeader", [rpc("chain_getFinalizedHead", [])])
            with db:
                db.execute("BEGIN IMMEDIATE")
                db.execute("UPDATE watermark SET height=max(height,?) WHERE id=1",
                           (int(final["number"],16),))
            time.sleep(2)
