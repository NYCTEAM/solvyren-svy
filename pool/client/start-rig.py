#!/usr/bin/env python3
"""Start a Solvyren Linux rig with a public wallet and worker name."""
import argparse
import os
import re
import tempfile

from hive_rig import run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wallet", help="SVY mainnet public payout address")
    parser.add_argument("--worker", help="Rig name (1-48 ASCII letters, digits, _ or -)")
    parser.add_argument("--pool", metavar="tls://HOST:PORT",
                        help="Mainnet pool URL (default: BOOT3)")
    args = parser.parse_args()
    if args.pool:
        os.environ["SMW_POOL_URL"] = args.pool
    wallet = args.wallet if args.wallet is not None else input("SVY mainnet payout address: ").strip()
    worker = args.worker if args.worker is not None else input("Worker name: ").strip()
    if not re.fullmatch(r"(?:SVY)?5[1-9A-HJ-NP-Za-km-z]{47}", wallet):
        parser.error("Invalid mainnet payout address")
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,48}", worker):
        parser.error("Invalid worker name")
    with tempfile.NamedTemporaryFile(mode="w") as config:
        config.write(f"{wallet}.{worker}\nall\n")
        config.flush()
        run(config.name)


if __name__ == "__main__":
    main()
