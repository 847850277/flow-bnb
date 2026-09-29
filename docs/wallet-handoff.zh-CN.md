# MCP 交易请求交接与本地开发钱包

[English](wallet-handoff.md) | 简体中文

这部分功能可以在没有个人钱包、私钥、Binance API Key 和真实资金的情况下开发、测试。目前已实现 JSON-RPC 开发钱包适配器、人工确认入口，以及离线的进程级演示。

**验证范围：**演示运行真实的 MCP stdio 服务、CLI 终端确认、钱包适配器进程和 Flow 回执检查，但 API 与 RPC 返回的是模拟数据。它没有进行密码学签名、执行 EVM、调用大模型，也没有完成主网交易。配置真实 Anvil/Hardhat 节点后，签名由节点负责；这一点尚未通过本演示进行真实节点验证。

## 运行演示

在仓库根目录执行：

```sh
python3 scripts/demo_handoff.py
```

脚本会构建二进制程序，启动仅本机可访问的模拟 RPC 服务，通过真实 MCP 服务提交交易请求，然后打开操作员 CLI。检查终端显示的模拟交易，输入完整的确认 ID 表示同意，输入其他内容表示拒绝。退出时会清理临时测试数据和日志。

这个演示脚本不接受生产环境端点或私钥。

自动验证使用同一套离线测试数据：

```sh
python3 scripts/demo_handoff.py --self-test
python3 scripts/demo_handoff.py --self-test --no-build --scenario wallet-reject
python3 scripts/demo_handoff.py --self-test --no-build --scenario timeout
```

`--no-build` 复用已构建的二进制程序。可选场景如下：

| 场景 | 验证内容 |
| --- | --- |
| `success` | 确认、提交、回执和余额变化的完整模拟流程 |
| `decline` | 人工拒绝后不提交交易 |
| `wallet-reject` | 钱包拒绝时保留失败状态，不自动重试 |
| `timeout` | RPC 接受提交后超时，保留结果不确定状态，不重复提交 |
| `expired` | 交易准备结果过期后阻止执行 |
| `wrong-chain` | 钱包节点链 ID 不匹配时阻止提交 |
| `cancel` | 取消尚未被操作员领取的请求 |

增加 `--output /new/file.json` 可保存标明模拟性质的验证结果；目标文件必须尚不存在。`--self-test` 只在这些模拟场景中通过伪终端输入确认 ID，不是生产环境的无人值守批准选项。

## MCP 与人工执行如何衔接

操作员配置专用目录，与生成的 YAML 分开存放：

```sh
mkdir -m 700 /absolute/path/to/trade-inbox
export FLOW_BNB_HANDOFF_DIR=/absolute/path/to/trade-inbox
cargo run --locked --bin flow-bnb-mcp -- --root /path/to/project
```

新增三个 MCP 工具：

| 工具 | 功能 |
| --- | --- |
| `request_trade_execution` | 保存待人工审核的 `TradeRequest`，返回请求 ID |
| `get_trade_execution` | 查询持久化状态，以及筛选后的交易哈希、回执成功标记、余额变化和模拟／开发环境标签 |
| `cancel_trade_execution` | 取消尚未被操作员领取的请求 |

**MCP 没有批准或签名工具。**工具参数不能指定策略文件、可执行程序、钱包端点或 `operator_confirmed` 标记。`prepare_trade` 仍然只读，返回的报告不授予执行权限。队列接收交易请求，不接收可直接执行的交易载荷或序列化的 `PreparedTrade`。

操作员先查看请求：

```sh
flow-bnb review-trade --handoff-dir /absolute/path/to/trade-inbox --intent-id ID
```

然后使用内置开发钱包入口执行：

```sh
flow-bnb approve-trade \
  --handoff-dir /absolute/path/to/trade-inbox --intent-id ID \
  --policy /absolute/path/to/policy.json \
  --wallet-config /absolute/path/to/dev-wallet.json
```

程序重新读取操作员的策略，获取**新的余额、报价、交易构建和模拟结果**，随后在 `/dev/tty` 中要求输入新的确认 ID。队列里的请求 ID 不是批准凭证，旧的交易准备结果不能复用。显式传入 `--demo` 时，API 数据会替换为固定测试数据，审计结果标记为 `simulation`。

## 状态、持久化与防重复执行

目录权限为 `0700`，请求和日志文件权限为 `0600`。程序使用 `create_new` 独占创建日志文件，保证不同进程不能同时领取同一个请求。取消和执行竞争同一个领取入口，只有一方能成功。

已取消或已领取的请求 ID 不能重复执行，重启后也一样。日志只追加，并在交给钱包前同步到磁盘；审计记录保留交易哈希和结算观察结果。日志不完整时按结果未知处理，不会重置为待执行。

| 状态 | 含义 |
| --- | --- |
| `completed_simulation` | 模拟流程完成，不代表真实链上交易 |
| `not_submitted` | 在交给钱包之前停止，例如拒绝确认或准备结果过期 |
| `cancelled` | 请求已取消，不能再次执行 |
| `needs_attention_outcome_may_be_unknown` | 已进入钱包调用边界，调用方不能可靠确定是否广播，需要人工核查 |

签名器超时或非零退出会保守地归入 `needs_attention_outcome_may_be_unknown`。即使测试数据明确模拟了拒绝，通用进程接口也不能仅凭退出码推断广播状态。**程序不会自动重试。**创建新请求前应先检查钱包和链上记录。

防重复机制覆盖同一个请求 ID，不会判断两个不同 ID 是否表达同一笔交易。旧记录可由操作员归档，MCP 没有删除记录的工具。

这些机制分离了 MCP 能力与操作员批准权限，但不是操作系统沙箱。如果 Agent 在同一用户下拥有不受限的 shell 权限，它仍能访问该用户的文件和终端。需要防范这种访问时，应单独部署批准入口和钱包权限。

## 内置开发节点钱包

币安 App 手机钱包使用独立的本机扫码适配器，见[手机钱包接入](mobile-wallet.zh-CN.md)。
`approve-trade --signer /绝对路径/签名器 --rpc-url https://...` 可以替代 `--wallet-config`；
手机模式成功状态为 `completed_wallet`，不能使用 `--demo`。真实手机和主网交易尚待验收。

```sh
flow-bnb-wallet-rpc --config /absolute/path/to/dev-wallet.json
```

适配器通过标准输入接收 `flow-bnb-signer-v1` JSON 协议，通过标准输出返回确认 ID 和交易哈希。Flow 不加载私钥；解锁的开发账户和 nonce 分配由节点管理。

配置示例：

```json
{
  "development_only": true,
  "rpc_url": "http://127.0.0.1:8545",
  "account": "0xYOUR_DEVELOPMENT_ACCOUNT",
  "max_gas": 200000,
  "max_gas_price_wei": 2000000000,
  "max_fee_wei": "400000000000000"
}
```

将 `account` 替换为开发节点账户。节点的链 ID 必须设为 `56`，以匹配目前面向 BSC 的交易流程；本地节点使用这个 ID 不意味着连接了主网。账户必须出现在 `eth_accounts` 返回值中。

适配器只接受以数字 IPv4 表示的本机回环 HTTP 地址，并检查客户端标识是否为支持的 Anvil、Hardhat 或模拟节点。这些是开发环境限制，不构成对节点身份的可信证明；应连接自己控制的本地开发节点，不应使用通往有真实资金钱包的生产代理。

提交前会核对发送方、接收方、calldata、原生币转账值为零、链 ID 和截止时间，再调用 `eth_estimateGas` 与 `eth_gasPrice`。Gas 估算增加 20% 余量，同时受 Gas 数量、单价和总费用上限约束。通过检查后只调用一次 `eth_sendTransaction`，错误或超时后不重试。

HTTP 重定向被禁用，Binance 请求头不会发送到钱包 RPC；父进程在启动适配器前清空其环境变量。

节点签名与广播的接口见 [Ethereum JSON-RPC 文档](https://ethereum.org/developers/docs/apis/json-rpc/#eth_sendtransaction)。当前内置适配器是开发节点适配器，Agentic Wallet、MetaMask 和生产硬件钱包仍需单独接入。

## 还需要完成什么

- 配置真实大模型客户端，并验证自然语言驱动的操作过程；当前演示使用固定逻辑的 MCP 测试客户端。
- 验收已实现的币安 App 扫码适配器，完成真实手机确认交互。
- 验证真实授权与兑换模拟，完成小额主网交易验收。
- 补充 RFQ 类型化数据与供应商适配、异常恢复，以及基于事件的资产核算。

运行 `scripts/check.sh` 可执行 Rust 检查、YAML 编译和全部七个离线进程级场景。CI 在 Rust 1.90/Linux 上执行相同场景。
