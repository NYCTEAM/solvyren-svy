# 第三方独立 SMW 矿池（开发预览）

运营者提供自己的域名、TLS 证书、独立节点、区块奖励钱包和费率。使用普通矿机管理软件上架 BOOT2、BOOT3 请看[官方池指南](../docs/pool-listing.md)。这里的服务端使用 SMW/1 over TLS。主网独立出块和独立钱包自动到账的端到端验收尚未完成。本分支供隔离联调，当前公开的节点二进制不能直接用作独立池出块节点。

## 费率和区块归属

配置见 [config.example.json](server/config.example.json)。`feeBps=100` 表示 1%，25 表示 0.25%，0 表示免费。费率适用于该池自己获得的普通 GPU 区块奖励，官方附加费为零。节点需要把区块的受益人设为运营者独占的 `rewardWallet`；由最终确认区块的 `powRewards.Issued` 事件确定归属，AI 额度不参与计算。

只在本池赢得且最终确认的区块中，按该轮同一高度和父块下各钱包已接受的工作量分配收益。先以最大余数法分完最小单位，再从每个钱包的毛额向下取整计算服务费。区块、每个钱包的毛额/净额/手续费及应付余额在同一笔 SQLite 事务内记账。零份额的自有区块会暂停供运营者对账。外池区块只推进游标。启动后配置和账本绑定，禁止通过修改配置文件回溯调整费率。

## 独立节点构建

使用本分支的源代码和仓库中的主网规格。在仓库根目录运行：

```sh
SKIP_WASM_BUILD=1 cargo build --locked --release -p eagle-node --features svy-independent-pool
install -m 0755 target/release/eagle-node /opt/svy-pool/eagle-node
install -m 0644 network/mainnet.raw.json /etc/svy-pool/mainnet.raw.json
```

先创建 /etc/svy-pool 和 /opt/svy-pool。创建系统用户与同名组 svynode；在 /etc/svy-pool/node.env 中配置 SVY_POOL_REWARD_ADDRESS 为运营者独占的公开区块奖励地址，必须与 config.json 的 rewardWallet 一致。使用 [节点服务](server/svy-pool-node.service) 启动归档全节点，生成自己的 P2P 身份，等待同步。在此模式下节点导出本地任务并再次验证 GPU 提交的获胜证明。运营者无需官方节点的身份、官方钱包凭据或投票密钥。该节点使用现有 SMW 共识和最终确认规则。

## Linux 环境

需要 systemd、Python 3.9+、Node.js 24+ 和运营者自己的归档 RPC。将本目录安装到 `/opt/svy-pool`，在该目录执行 `npm ci --omit=dev --ignore-scripts`。以下服务分开运行：

1. 建立系统用户和同名组 `svypool`、`svypay`。节点与验算服务共享独立的 `/run/svy-pool`，使节点能写入 `job.json`、读取及移除 `seal.bin`，使验算服务能读取任务和原子发布获胜证明；将 server/tmpfiles.conf 安装为 /etc/tmpfiles.d/svy-pool.conf，并运行 systemd-tmpfiles --create。节点需要 svypool 附加组。节点和验算服务使用相同的本机安全 RPC（默认 HTTP 9954）。验证器检查任务的主网创世哈希、算法、目标和 Share 证书。
2. 创建 `/var/lib/svy-pool-shares`，属主属组 `svypool:svypool`、权限 2750。运行 `sudo -u svypool sh -c 'umask 027 && python3 /opt/svy-pool/server/init_db.py'`。保证 `svypay` 能以只读方式访问 SQLite 及 WAL 共享内存文件。
3. 为自己的矿池域名设置 DNS 和受客户端信任的 TLS 证书。将证书链和私钥分别放入 `/etc/svy-pool/fullchain.pem`、`/etc/svy-pool/key.pem`。使用 systemd LoadCredential 把凭据交给入口进程。服务器仅需对矿工开放 TCP 18444。证书续期后重启 edge 服务。
4. 从示例生成 `/etc/svy-pool/config.json`。填写两个公开地址，将 `startHeight` 改成运营者开始核算的首个区块，为归档节点配置只供本机访问的 WebSocket RPC。使用独立的区块收款钱包，避免其他业务向同一地址出块或从它签发交易。测试环境和生产环境各用独立的账本、钱包和节点身份。
5. 将 `server/svy-pool*.service` 和 `server/svy-pool.socket` 安装到 `/etc/systemd/system`，执行 `systemctl daemon-reload`。在隔离环境准备好本地模板生产器后启动 `svy-pool.socket`、`svy-pool-seal.service`、`svy-pool-edge.service`、`svy-pool-settlement.service`。默认只记账。签名器使用 systemd 服务覆盖项 `LoadCredential=payout.seed:/etc/svy-pool/payout.seed`；开启前核对它对应 `rewardWallet`，并留足网络手续费及账户保活余额。签名凭据仅由结算用户读取。

付款门槛为每地址 1 SVY。签名后先持久化交易，再广播。由最终确认的交易哈希、成功事件、转出地址、收款地址及精确金额核销。失败的已上链交易需人工对账。若运营费钱包等于区块奖励钱包，服务费直接保留在该钱包，在账本中单独记录。备份使用一致的 SQLite 在线备份；恢复旧备份后先核对链上付款再开启签名。

## 矿机接入

以[官方下载页](https://solvyren.com/downloads/smw/)的 Linux x64 多卡预览包 1.2.1 为基础，核对官网 SHA-256 后解压。将本目录 `client/*.py` 覆盖到解压后的 `solvyren-smw/` 目录，保留其中的 CUDA 程序、OpenCL 内核、驱动配套脚本及官方池证书。在目录内运行：

```sh
python3 start-rig.py --pool tls://pool.example.org:18444 --wallet "$SVY_WALLET" --worker rig01
```

替换示例域名和公开钱包。独立池使用系统信任链校验域名，BOOT2、BOOT3 继续使用客户端附带的证书。现有 Windows EXE 和历史 HiveOS 包尚未纳入本次独立域名验收。

## 本地验证

```sh
node --test server/ledger.test.mjs
python3 -B tests/gpu_smoke.py /path/to/unpacked/solvyren-smw --max-watts 250
```

第二项需要 NVIDIA Linux 或 WSL CUDA 环境、OpenSSL 和已设置功耗上限的空闲显卡。它生成仅用于本次测试的 localhost 证书、隔离模板和临时账本，使用真实 GPU 及服务端证明校验，不发送链上交易。生产验收还需要独立域名 TLS、独立节点获得最终确认的区块、矿工净收益及运营者自定费率的链上到账。
