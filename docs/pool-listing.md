# 第三方平台上架 SVY 官方矿池

面向显卡矿机管理平台和矿池导航站。平台可以在自己的币种列表展示 Solvyren（SVY），让矿工连接官方矿池。有效 Share 由官方矿池核验、按用户填写的主网收款地址记账和结算。[官方矿池页面](https://pool.solvyren.com/)公示连接地址、实时算力和费率。

| 上架字段 | 配置值 |
| --- | --- |
| 币种 / 网络 | SVY / Solvyren Mainnet |
| GPU 算法 | SMW-16/2048 v1 |
| 矿池协议 | SMW/1 over TLS |
| 统一入口（推荐） | `tls://pool.solvyren.com:18444` |
| 香港 BOOT2（直连） | `tls://boot2.solvyren.com:18444` |
| 美国 BOOT3（直连） | `tls://boot3.solvyren.com:18444` |
| 用户填写 | SVY 主网公开收款地址、矿工名（1–48 个英文字母、数字、下划线或连字符） |

统一入口优先使用香港 BOOT2。当 BOOT2 不可用时，矿工重连后自动接入美国 BOOT3，仍按同一收款地址结算。如果矿机平台分开要求主机、端口和加密，填写 `pool.solvyren.com`、`18444`，并开启 TLS。

## 矿池服务费

官方美国和香港入口共用同一套结算。从主网区块 **#19150** 起，每个最终确认区块先按有效贡献分配毛收益，然后对每个钱包在官方池获得的 GPU 毛收益收取 **0.25%**。同一钱包的多台矿机合并计算。服务费按最小单位向下取整，余数归矿工。例如，100 SVY 毛收益对应矿工 99.75 SVY、官方池 0.25 SVY。

从主网区块 **#22661** 起，官方云服务器套餐的挖矿毛收益也按 **0.25%** 收取服务费。同一账户在该区块的多份套餐先合并计算费额。费率按收益所属区块生效。

自主网区块 **#22623** 起，新产生的官方池服务费记入 `5E4QL7QoJXBjJ4vn2brDowCgFgbVB3av7NgGXSTzB2zxdQrG`。#19150 至 #22622 的服务费归原地址 `5CHsAszd1upWWfo6Dc54BJHLXjg7Zb94fsi5Y57mQyXVVZj2`。服务费独立记账，累计达到 0.01 SVY 后进入自动付款队列。同一账户两次付款至少间隔六个区块，矿工收益的自动付款门槛为 1 SVY。

第三方自建并独立结算的矿池自行公布费率和收款地址，官方附加费为零。例如第三方池收费 1%，100 SVY 毛收益中矿工得 99 SVY，该池得 1 SVY。连接官方统一入口或官方直连入口，均适用官方池费率。

使用自有域名并独立出块、设置费率和结算的运营者，可查看[第三方独立矿池开发预览](https://github.com/NYCTEAM/solvyren-svy/tree/pool/independent-smw/pool)。预览包的真实 GPU 证明已在隔离环境验收，第三方独立出块与付款仍需主网端到端验收。

## Linux x86-64 自定义矿工

平台支持运行自定义 Linux 程序时，在模板中配置[官方 AMD / NVIDIA 多卡预览包 1.2.3](https://solvyren.com/downloads/smw/Solvyren-SMW-1.2.3-Linux-x64-MultiGPU-Preview.tar.gz)，用[官方 SHA-256 清单](https://solvyren.com/downloads/smw/SHA256SUMS)校验后解压。按下载页准备驱动和 Python 依赖，并在矿机系统中设置显卡功耗上限。

工作目录设为解压后的 `solvyren-smw`。把下例的钱包和矿工名变量映射到平台字段，并将各值作为独立参数传递：

```sh
./start-rig.py --pool tls://pool.solvyren.com:18444 --wallet "$SVY_WALLET" --worker "$WORKER_NAME"
```

Linux / HiveOS 1.2.3 和 Windows NVIDIA 1.3.1 默认连接统一矿池。平台可预填统一入口，用户填写自己的收款地址和矿工名。请勿预填运营方钱包。HiveOS Custom Miner 的 Miner name 为 `solvyren-smw`，Installation URL 使用 [HiveOS 1.2.3 安装包](https://solvyren.com/downloads/smw/solvyren-smw-1.2.3.tar.gz)，Pool URL 设为 `tls://pool.solvyren.com:18444`。

## 接入验收

用独立矿工名启动，确认矿池回报的已接受 Share 持续增加，然后按用户地址在[主网浏览器](https://scan.solvyren.com/explorer)核对结算到账。已使用 RTX 3090 验证统一入口从 BOOT2 切换至 BOOT3，切换前后均收到有效 Share 回执。AMD 真机 Share 和到账仍待验证。

仅支持通用 Stratum 的软件需要另行适配 SMW/1，并具备 SMW-16/2048 v1 计算能力。第三方平台是否能直接导入，取决于该平台的自定义矿工接口。
