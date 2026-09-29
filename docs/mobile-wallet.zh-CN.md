# 币安 App 手机钱包接入

已实现本机连接页和 `flow-bnb-signer-v1` 适配器，使用币安官方
[`@binance/w3w-ethereum-provider`](https://developers.binance.com/en/docs/products/web3-connect/evm-compatible-provider)
SDK 显示扫码弹窗。无需 WalletConnect project ID，也不需要导出私钥或助记词。
目前通过了模拟钱包 / RPC 的自动化测试和浏览器扫码弹窗检查；真实手机连接、授权、兑换和主网成交仍待用户验收。

## 1. 启动连接页

需要 Node.js 22 或更高版本。在项目根目录执行：

```sh
npm ci --ignore-scripts --prefix wallet-mobile
npm run build --prefix wallet-mobile
npm start --prefix wallet-mobile -- --account 0xYOUR_BSC_ADDRESS
```

在**电脑浏览器**打开启动日志中的完整本机链接，点击“连接币安手机钱包”，
用手机币安 App 钱包扫描 SDK 弹窗中的二维码并确认连接。
确认页面显示预期地址和 BSC 主网；扫码只建立连接，不请求签名。
手机通过 SDK 中继连接，不需要访问电脑的 `127.0.0.1` 地址。

保持服务、浏览器和手机钱包在线。完整链接包含会话凭据，不要分享或录入公开视频；
页面加载后会从地址栏移除凭据，刷新时需重新打开启动日志里的完整链接。
服务仅监听 `127.0.0.1`，不用公网隧道。服务重启会生成新的链接和签名器路径。

## 2. 重新准备并人工确认一笔交易

先把 `examples/nvda-probe.json` 的公开占位地址替换为自己的地址，保存为本地请求。
按实际意愿核对代币和额度，设置可信的本地策略。示例策略的 router / spender 白名单为空，
不会因接入钱包而自动放行。不要直接复制报价响应里的地址来绕过策略失败。

在另一个交互式终端设置好 Binance API 环境变量，然后执行：

```sh
cargo run --locked -- execute-trade \
  --request /absolute/path/to/my-trade.json \
  --policy /absolute/path/to/my-policy.json \
  --report /absolute/path/to/new-run.json \
  --signer /absolute/path/printed/by/mobile-server/flow-bnb-wallet-mobile \
  --rpc-url https://bsc-dataseed.bnbchain.org
```

`--signer` 使用手机连接服务实际打印的绝对路径；报告路径必须尚不存在。
CLI 重新查询报价、模拟并检查策略，用户在终端输入完整 `confirmation_id`。
随后连接页展示单笔交易，用户点击“发送到手机确认”，再在手机核对并确认。
授权交易会额外显示 spender 和额度；兑换展示目标合约和完整交易参数。
CLI 获取哈希后会核对实际交易载荷、回执和结算观察。

**当前确认窗口从准备开始最多 30 秒。** 所以必须先连好手机，再运行执行命令。
如果来不及确认，不要继续批准手机上的过期请求；先拒绝并核查记录。
适配器不会延长报价有效期。授权完成后需重新准备兑换，每笔独立确认。
当前只支持 BSC 上原生 value 为零的 ERC-20 SWAP / 精确额度授权，不支持 RFQ 签名或 BNB 转账。

## 3. MCP 请求也可以交接到手机

对已通过 MCP 排队的 intent，使用同一手机签名器：

```sh
cargo run --locked -- approve-trade \
  --handoff-dir /absolute/path/to/inbox \
  --intent-id INTENT_ID \
  --policy /absolute/path/to/my-policy.json \
  --signer /absolute/path/printed/by/mobile-server/flow-bnb-wallet-mobile \
  --rpc-url https://bsc-dataseed.bnbchain.org
```

`--signer` 与开发节点的 `--wallet-config` 二选一；手机模式不能使用 `--demo`。
MCP 仍无直接批准 / 签名工具，必须经过终端人工确认。成功状态为 `completed_wallet`。

## 费用与未知结果

适配器用 HTTPS RPC 校验链并估算 Gas，使用估算值的 120%；默认 Gas 上限 600000，
Gas price 上限 1 Gwei，按提交参数计算的单笔费用上限 0.0001 BNB。
启动参数 `--rpc`、`--max-gas`、`--max-gas-price-wei`、`--max-fee-wei` 可调整这些设置。
超过任一上限会在交给手机前阻止交易。nonce 由钱包管理，手机上最终显示的费用仍需本人核对；
钱包或用户可能修改费用，适配器不能强制远端钱包的行为。

签名器进程使用绝对 Node 解释器路径，可在 Rust 清空环境变量后运行。
本机使用不同凭据隔离浏览器与签名器请求，并检查 Host、Origin、账户、链、
有效期和载荷。它是同一用户下的受信任本地组件，不是对本机恶意进程的沙箱。

审计记录默认位于 `.flow-bnb/mobile-wallet/requests/`，请求在交给手机前落盘。
同一个确认 ID 无法重放。已经交给钱包但超时、拒签、断连或返回无效哈希时，
保守记录为 `unknown`，本次服务会阻止后续提交；迟到的哈希仍会保留。
**断开连接不能保证撤销手机上已收到的请求。** 请先检查手机和链上记录，再决定是否重启服务和重新准备。
重启不会清除旧 ID 的防重放记录，但允许新的确认 ID，不能把重启当作自动重试。

## 开发验证

```sh
npm run build --prefix wallet-mobile
npm test --prefix wallet-mobile
bash scripts/check.sh
```

Node 测试使用模拟 provider / RPC，覆盖精确载荷、单次提交、清空环境的真实签名器进程、
错误账户 / 链、费用限制、来源校验、过期、拒签、迟到哈希和持久化防重放。
Rust 和离线交接检查保留原有开发钱包路径；这些测试均不转移主网资金。
