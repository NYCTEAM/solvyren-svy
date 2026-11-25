import sqlite3
import os
from pathlib import Path

if __name__ == "__main__":
    path = Path(os.environ.get("SVY_SHARE_DB","/var/lib/svy-pool-shares/shares.sqlite"))
    with sqlite3.connect(path) as db:
        db.executescript("""
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS watermark(id INTEGER PRIMARY KEY CHECK(id=1), height INTEGER NOT NULL);
            INSERT OR IGNORE INTO watermark VALUES(1,0);
            CREATE TABLE IF NOT EXISTS gpu_stream_shares(
                challenge TEXT NOT NULL, nonce TEXT NOT NULL, height INTEGER NOT NULL,
                parent_hash TEXT NOT NULL, worker_id TEXT NOT NULL, wallet TEXT NOT NULL,
                work INTEGER NOT NULL CHECK(work=65536), PRIMARY KEY(challenge,nonce));
            CREATE INDEX IF NOT EXISTS round_lookup ON gpu_stream_shares(height,parent_hash,wallet);
        """)
