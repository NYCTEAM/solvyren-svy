# SMW/1 矿工连接

矿工与矿池域名建立 TLS 1.2 或以上连接，验证证书链和主机名。每条消息是以换行结束的 UTF-8 JSON 对象，单次请求最多 8192 字节。矿机上的每张显卡独立连接。矿工只提交公开收款地址及矿工名。

1. 首条请求包含 `protocol`（`SMW/1`）、`wallet`（SVY 主网公开地址）、`worker`（1–48 位英文字母、数字、下划线、连字符）。
2. 响应包含 `protocol`、标准化的 `wallet`、带钱包前缀的 `worker`、`job`、`start`、`target` 和 `receipts`。任务中的 `genesis`、`height`、`parentHash`、`preHash`、`workAlgorithm` 及 `chainTarget` 来自节点模板。客户端应验证主网标识和 SMW 算法标识。
3. `start` 是会话的 64 位无符号起始 nonce，用能精确表示 64 位整数的 JSON 解析器读取。搜索范围是从 start 到 start + 2³² − 1。顶层 `target` 为 64 个小写十六进制字符，表示 Share 目标；区块目标在 `job.chainTarget` 中。
4. 矿工发送下一条请求，携带本轮 `preHash` 和 `proofs` 数组（最多 32 份，每份为 105 字节的小写十六进制证书）。空数组可用于轮询。服务器逐项返回 `accepted`、`late` 或 `duplicate`；收到更新的任务后改用新模板和 nonce 区间。断线重连须重新登录。
5. 服务端按主网挑战和 nonce 重算证书，符合区块目标的证明交给本地节点再次验证。最终确认和收益分账由[运营者结算服务](README.md)处理。返回 accepted 表示 Share 已入账，链上付款有独立的最终确认记录。

字段编码和挑战计算见[参考实现](server/smw_share.py)，客户端收发流程见[NVIDIA 客户端](client/miner.py)。
