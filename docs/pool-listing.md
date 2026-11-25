# 第三方平台上架 SVY 官方矿池

面向显卡矿机管理平台和矿池导航站。平台可以在自己的币种列表展示 Solvyren（SVY），让矿工连接官方矿池。有效 Share 由官方矿池核验、按用户填写的主网收款地址记账和结算。仅展示矿池的站点可直接链接[官方下载与配置页](https://solvyren.com/downloads/smw/)。

| 上架字段 | 配置值 |
| --- | --- |
| 币种 / 网络 | SVY / Solvyren Mainnet |
| GPU 算法 | SMW-16/2048 v1 |
| 矿池协议 | SMW/1 over TLS |
| 美国入口（默认） | `tls://boot3.solvyren.com:18444` |
| 香港入口 | `tls://boot2.solvyren.com:18444` |
| 用户填写 | SVY 主网公开收款地址、矿工名（1–48 个英文字母、数字、下划线或连字符） |

## 矿池服务费

官方美国和香港入口共用同一套结算。从主网区块 **#19150** 起，每个最终确认区块先按有效贡献分配毛收益，然后对每个钱包在官方池获得的 GPU 毛收益收取 **0.25%**。同一钱包的多台矿机合并计算。服务费按最小单位向下取整，余数归矿工。例如，100 SVY 毛收益对应矿工 99.75 SVY、官方池 0.25 SVY。

BOOT2 和 BOOT3 自主网区块 **#22623** 起，将新产生的官方池服务费记入 `SVY5E4QL7QoJXBjJ4vn2brDowCgFgbVB3av7NgGXSTzB2zxdQrG`。#19150 至 #22622 的服务费仍归原地址 `SVY5CHsAszd1upWWfo6Dc54BJHLXjg7Zb94fsi5Y57mQyXVVZj2`，按收益所属区块确定收款方。服务费独立记账，累计达到 0.01 SVY 后进入自动付款队列；同一账户两次付款至少间隔六个区块。矿工收益的自动付款门槛仍为 1 SVY。生效前的收益和云套餐分配均不收此费。

第三方自建并独立结算的矿池自行公布费率和收款地址，官方附加费为零。例如第三方池收费 1%，100 SVY 毛收益中矿工得 99 SVY，该池得 1 SVY。第三方平台若把矿机连接到 BOOT2 或 BOOT3，则适用官方池费率。

## Linux x86-64 自定义矿工

平台支持运行自定义 Linux 程序时，在模板中配置[官方 AMD / NVIDIA 多卡预览包 1.2.1](https://solvyren.com/downloads/smw/Solvyren-SMW-1.2.1-Linux-x64-MultiGPU-Preview.tar.gz)，用[官方 SHA-256 清单](https://solvyren.com/downloads/smw/SHA256SUMS)校验后解压。按下载页准备驱动和 Python 依赖，并在矿机系统中设置显卡功耗上限。

工作目录设为解压后的 `solvyren-smw`。下例的环境变量表示平台传入的字段，接入方应映射到自己平台的变量名，并将各值作为独立参数传递：

```sh
./start-rig.py --pool "$POOL_URL" --wallet "$SVY_WALLET" --worker "$WORKER_NAME"
```

平台可预选美国或香港入口，用户只填写自己的收款地址和矿工名。请勿预填运营方钱包。使用 HiveOS Custom Miner 格式的平台，可按[下载页的 HiveOS 配置](https://solvyren.com/downloads/smw/)选用专用包；1.2.0 的池地址使用 BOOT3。

## 接入验收

用独立矿工名启动，确认矿池回报的已接受 Share 持续增加，然后按用户地址在[主网浏览器](https://scan.solvyren.com/explorer)核对结算到账。RTX 3090 已通过香港入口的主网 Share 实测，AMD 真机 Share 和到账仍待验证。

仅支持通用 Stratum 的软件需要另行适配 SMW/1，并具备 SMW-16/2048 v1 计算能力。第三方平台是否能直接导入，取决于该平台的自定义矿工接口。
