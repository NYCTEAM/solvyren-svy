# Solvyren mainnet node release notes

The mainnet difficulty retarget activated at block **#24121**. Full nodes must run `mainnet-retarget-24121` or a compatible newer release.
Keep the same data directory and chain specification. Compare a finalized
block hash with [SVYscan](https://scan.solvyren.com/explorer) after the restart.

Download the executable, `mainnet.raw.json`, and `SHA256SUMS` from the
[node release](https://github.com/NYCTEAM/solvyren-svy/releases/tag/mainnet-retarget-24121).

## Linux

```sh
sha256sum --check --ignore-missing SHA256SUMS
gzip -dc solvyren-node-linux-x86_64.gz > solvyren-node
chmod +x solvyren-node
./solvyren-node --chain ./mainnet.raw.json --base-path ./svy-data --sync full --rpc-methods Safe --no-telemetry --listen-addr /ip4/0.0.0.0/tcp/30333
```

## Windows

Run PowerShell in the download directory and compare the hashes with
`SHA256SUMS` before starting.

```powershell
Get-FileHash .\solvyren-node-windows-x86_64.exe, .\mainnet.raw.json -Algorithm SHA256
.\solvyren-node-windows-x86_64.exe --chain .\mainnet.raw.json --base-path .\svy-data --sync full --rpc-methods Safe --no-telemetry --listen-addr /ip4/0.0.0.0/tcp/30333
```

Allow inbound TCP 30333 in the host firewall and cloud security group if the
node accepts incoming peers. Allow outbound TCP 30343 to reach the bootnodes.
Older non-validator builds use WebSocket listeners by default. The explicit
TCP listener prevents connection problems with the official bootnodes.

Mainnet uses SMW-16/2048 v1 MatMul PoW and GRANDPA finality. Mainnet is live.
AI inference and the production inference API are still closed.

The wallet endpoint accepts WSS at `wss://wallet.solvyren.com/wallet-rpc`
for the wallet origin. The public developer endpoint at `https://scan.solvyren.com/rpc`
accepts HTTPS JSON-RPC POST and exposes a subset of Safe methods.

The workspace declares the Unlicense. Vendored dependencies keep the
licenses identified in their source and in `LICENSES/`.
