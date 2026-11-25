# Solvyren mainnet node

## Required full-node upgrade before block 24,121

Install the rolling-retarget release before mainnet reaches block **24,121**.
Stop the existing node, replace its executable, then restart it with the same
chain specification, data directory, and launch options. Verify the genesis
hash and compare a finalized block hash with SVYscan after the restart.
Starting at block 24,121, validators recalculate difficulty for each block
from the work and block timestamps in the preceding 10 intervals. The
target remains a 10-second average. Mining clients can continue to use their
existing pool URL and proof protocol.

**主网全节点升级。** 请在区块 **#24121** 前更换节点程序。停止旧节点后，
使用原来的链规格、数据目录和启动参数重启。启动后核对创世哈希及最终确认区块。
从 #24121 起，主网按最近 10 个区块间隔的工作量和时间戳逐块计算难度，
目标平均出块时间为 10 秒。矿工可以继续使用原矿池地址及挖矿协议。

Run a full node to independently synchronize and verify the Solvyren SVY mainnet. Each node generates its own local network identity. A GPU and a payout wallet are optional for other roles and are not required to run a full node.

| Network | Value |
| --- | --- |
| Chain ID | `solvyren-svy` |
| Token | SVY · 12 decimals |
| Genesis block hash | `0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8` |
| SMW-16/2048 v1 activation | Block 10,000 |
| Block 10,000 hash | `0x94a4c4f1ff3cc7a78fdd50c60507e9bd965b8f53490c1fdbdf2bbc59900d9f11` |

## Run on Linux x86-64

Download the Linux executable archive, `mainnet.raw.json` and `SHA256SUMS` from the [node release](https://github.com/NYCTEAM/solvyren-svy/releases/tag/mainnet-retarget-24121). In the download directory run:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gzip -dc solvyren-node-linux-x86_64.gz > solvyren-node
chmod +x solvyren-node
./solvyren-node --chain ./mainnet.raw.json --base-path ./svy-data --sync full --rpc-methods Safe --no-telemetry --listen-addr /ip4/0.0.0.0/tcp/30333
```

## Run on Windows x86-64

Download the Windows executable, chain specification and checksums from the same release. Compare both file hashes with the corresponding entries in `SHA256SUMS` before starting:

```powershell
Get-FileHash .\solvyren-node-windows-x86_64.exe, .\mainnet.raw.json -Algorithm SHA256
.\solvyren-node-windows-x86_64.exe --chain .\mainnet.raw.json --base-path .\svy-data --sync full --rpc-methods Safe --no-telemetry --listen-addr /ip4/0.0.0.0/tcp/30333
```

## Verify synchronization

Keep the explicit TCP listen address in the launch command. Allow inbound TCP
30333 in the host firewall and cloud security group if the node should accept
incoming peers. Allow outbound TCP 30343 to reach the bootnodes in the raw
chain specification. Leave the local RPC listener on loopback.

**节点接入。** 保留命令中的 `--listen-addr /ip4/0.0.0.0/tcp/30333`。
允许出站 TCP 30343 以连接引导节点。若向其他节点提供入站连接，在防火墙及云安全组
放行入站 TCP 30333。节点本机的 RPC 保持在回环地址。

BOOT2 is the primary bootstrap peer. BOOT1 and BOOT3 are additional seeds.
To keep BOOT2 as a persistent peer, append the option below. The other public
peers can still be discovered.

```text
--reserved-nodes /dns4/boot2.solvyren.com/tcp/30343/p2p/12D3KooWEzpseawXDTCH7Pbd9sUsj6AdtgBehhSpjRz4oSCm6N26
```

### Public HTTPS JSON-RPC

Send a JSON-RPC 2.0 POST to `https://scan.solvyren.com/rpc`. The endpoint uses
the BOOT1 archive and falls back to BOOT2 for supported queries. Historical
state may require the archive. The gateway accepts read methods such as
`chain_getBlockHash`, `chain_getHeader`, `chain_getFinalizedHead` and
`state_getStorage`. Wallet traffic uses the WebSocket endpoint
`wss://wallet.solvyren.com/wallet-rpc`.

```sh
curl -sS https://scan.solvyren.com/rpc -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"chain_getBlockHash","params":[0]}'
```

From another terminal query the node:

```sh
curl -s http://127.0.0.1:9944 -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"chain_getBlockHash","params":[0]}'
curl -s http://127.0.0.1:9944 -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"system_syncState","params":[]}'
curl -s http://127.0.0.1:9944 -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"chain_getFinalizedHead","params":[]}'
```

Check the genesis hash above. Compare finalized block hashes at the same height with the [mainnet explorer](https://scan.solvyren.com/explorer).

## Build from source

The Rust workspace includes the native node, runtime, pallets, mining client, and the patched consensus dependency. The mainnet build embeds the genesis runtime Wasm from `artifacts/` and checks the raw specification fingerprint at startup. On Linux use Rust 1.89.0, Cargo, a C/C++ compiler, CMake, Clang/libclang, pkg-config and protoc.

```sh
SKIP_WASM_BUILD=1 cargo build --locked --release -p eagle-node --bin eagle-node --features svy-boot3-cpu
./target/release/eagle-node --chain ./network/mainnet.raw.json --base-path ./svy-data --sync full --listen-addr /ip4/0.0.0.0/tcp/30333
```

The build uses the checked-in genesis Wasm. SKIP_WASM_BUILD skips auxiliary SDK Wasm builds. Use full synchronization so the node executes and verifies historical blocks. The default role is a non-voting full node. Source-level regression tests and historical consensus rules are kept for verification of existing blocks. Licensing follows the crate manifests and source notices, including the vendored Substrate consensus crate.

## GPU mining

Set the Pool URL to `tls://pool.solvyren.com:18444` and enter your public SVY mainnet payout address and worker name. The unified endpoint uses SMW/1 over TLS. It prefers Hong Kong (BOOT2) and sends reconnecting miners to the United States (BOOT3) if BOOT2 is unavailable. Both servers share the same settlement ledger.

Windows NVIDIA 1.3.1 and Linux / HiveOS 1.2.3 use this endpoint by default.

| Region | Pool URL | Pool fee |
| --- | --- | --- |
| Unified (recommended) | `tls://pool.solvyren.com:18444` | 0.25% |
| Hong Kong (direct) | `tls://boot2.solvyren.com:18444` | 0.25% |
| United States (direct) | `tls://boot3.solvyren.com:18444` | 0.25% |

The official pool fee is 0.25% of gross GPU mining rewards from block 19,150 and of gross cloud plan mining rewards from block 22,661. For every 100 SVY earned, the miner receives 99.75 SVY. [Official pool dashboard](https://pool.solvyren.com/) · [Download the mining client](https://solvyren.com/downloads/smw/)

[Integration guide for third-party mining platforms (中文)](docs/pool-listing.md)
