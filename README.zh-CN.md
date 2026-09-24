# flow-bnb

[English](README.md) | 简体中文

基于 [`postman-flow`](https://github.com/847850277/postman-gpui/tree/main/crates/postman-flow) 的 BNB Chain 工作流工具。用 YAML 描述 API 调用、结果校验、数据传递和循环等待，让 CLI 或 Agent 执行可检查、可追踪的流程。

项目以 BNB Hack Tokenized Stocks 方向的 SDK / 交易 Agent 基础设施为目标。目前可以查询股票代币、读取钱包资产、获取兑换报价、构建并模拟交易，以及跟踪已广播交易的回执。已提供 MCP 请求交接、人工确认和本地开发节点钱包适配器；**生产钱包接入和真实主网交易尚未完成验收。**

## 项目如何分工

`postman-flow` 负责通用工作流执行，包括 HTTP 请求、表达式、断言、输出和循环。`flow-bnb` 负责 Binance Web3 API 鉴权、BNB Chain 业务模板、交易策略、回执适配器和 MCP 工具。

引擎通过公开 Git 提交固定版本，构建时使用 `--locked`，方便复现依赖。

## 已有的四个流程

| 文件 | 执行过程 | 所需配置 |
| --- | --- | --- |
| [rwa_discovery.http.yml](flows/rwa_discovery.http.yml) | 查询发行平台 → 搜索股票代币 → 获取代币价格和股票参考价格 | Binance Web3 API Key / Secret |
| [wallet_snapshot.http.yml](flows/wallet_snapshot.http.yml) | 查询钱包代币余额 → 查询最近交易 | API Key / Secret、钱包地址 |
| [safe_swap_preparation.http.yml](flows/safe_swap_preparation.http.yml) | 获取兑换报价 → 构建未签名交易 → 模拟交易 | API Key / Secret、钱包地址、代币地址和数量 |
| [transaction_receipt.http.yml](flows/transaction_receipt.http.yml) | 校验 RPC 链 ID → 循环查询回执 → 等待目标确认数 | RPC 地址、已广播交易哈希 |

四个 YAML 都带有中文注释，可以直接查看每一步的用途。

## 快速开始

最低支持 Rust 1.90；仓库的本地工具链配置选择 Rust 1.97，CI 同时验证 Rust 1.90。下面的命令均在 `flow-bnb` 仓库根目录执行。

### 1. 离线检查 YAML

`check` 只解析和编译流程，不发送请求，也不需要密钥：

```bash
cargo run --locked -- check flows/rwa_discovery.http.yml
cargo run --locked -- check flows/wallet_snapshot.http.yml
cargo run --locked -- check flows/safe_swap_preparation.http.yml
cargo run --locked -- check flows/transaction_receipt.http.yml
```

编译通过表示流程定义有效，实际 API 是否可用、鉴权是否成功、业务断言是否成立，需要运行时验证。

### 2. 配置 Binance Web3 凭据

在 [Binance Web3 开发者平台](https://web3.binance.com/en/dev-portal) 创建凭据，然后设置环境变量：

```bash
export BINANCE_WEB3_API_KEY='your-api-key'
export BINANCE_WEB3_SECRET_KEY='your-secret-key'
```

也可以使用仓库提供的 `.env.example`。以下命令仅在 `.env` 不存在时创建它：

```bash
test -f .env || cp .env.example .env
```

编辑 `.env`，填入自己的配置，然后在当前终端加载：

```bash
set -a
source .env
set +a
```

**CLI 不会自动读取 `.env`。** 每次新开终端，都需要重新加载，或由运行环境注入变量。`.env` 已被 Git 忽略，真实凭据不要写进 YAML 或 `.env.example`。

请求使用 HMAC-SHA256 / Base64 签名。凭据只会附加到允许的 Binance Web3 HTTPS API 请求，传输层禁用重定向。

### 3. 运行股票代币查询

```bash
cargo run --locked -- run flows/rwa_discovery.http.yml \
  --input ticker=NVDA \
  --input platform_id=ondo
```

这个例子查询 Ondo 平台的 NVDA 股票代币，输出公司名称、代币符号、链 ID、合约地址、代币价格、股票参考价格和价格更新时间。

当前模板取搜索结果的第一条记录、其中的第一个链上资产，并要求链 ID 为 `56`（BSC）。如果不符合就失败，不会自动遍历其他候选资产。它只获取价格，不会计算价差或自动买卖。

输入也可以从环境变量获取：

```bash
export FLOW_BNB_TICKER=NVDA
cargo run --locked -- run flows/rwa_discovery.http.yml \
  --env ticker=FLOW_BNB_TICKER \
  --input platform_id=ondo
```

`--input 名称=值` 直接传值；`--env 输入名称=环境变量名` 从指定环境变量读取。

## 用 -v 查看 HTTP 过程

`-v` 和 `--verbose` 等价，可以查看实际请求和响应：

```bash
cargo run --locked -- run flows/rwa_discovery.http.yml \
  --input ticker=NVDA \
  --input platform_id=ondo \
  -v
```

详细日志包括 HTTP 方法、URL、请求头、请求体、响应状态、响应头、响应体和耗时。它们写到 **stderr**，流程事件仍写到 stdout。

单独保存 HTTP 日志：

```bash
cargo run --locked -- run flows/rwa_discovery.http.yml \
  --input ticker=NVDA --input platform_id=ondo \
  --verbose 2>http.log
```

API Key 和签名请求头显示为 `[REDACTED]`，Secret Key 不记录到日志；引擎会对声明为敏感的输入和已知敏感输出进行脱敏。响应体仍可能包含钱包或业务数据，分享日志前请检查内容。

不加 `-v` 时使用默认输出。`watch-transaction` 同样支持该参数，RPC URL 会脱敏，stdout 保持为 JSON 报告。

## 查询钱包资产和最近交易

将示例地址替换为实际 BSC 钱包地址：

```bash
cargo run --locked -- run flows/wallet_snapshot.http.yml \
  --input wallet_address=0xYourBscAddress \
  -v
```

只需要钱包地址和 API 凭据，不需要钱包私钥。当前查询代币余额的第一页，最多 100 条，并排除风险代币；最近交易最多 20 条。此模板尚未实现自动翻页，也不负责等待交易确认。

## 获取报价、构建并模拟交易

替换下面的代币地址和钱包地址后运行：

```bash
cargo run --locked -- run flows/safe_swap_preparation.http.yml \
  --input from_token_address=0xSellToken \
  --input to_token_address=0xTokenizedStock \
  --input amount=1000000 \
  --input wallet_address=0xYourBscAddress \
  --input slippage_percent=0.5 \
  -v
```

`amount` 使用卖出代币的最小单位。例如，只有当代币精度为 6 位时，`1000000` 才表示 1 个代币。`slippage_percent=0.5` 表示 0.5% 滑点。

流程选择首条报价，构建未签名的 `from / to / value / data`，再调用模拟接口，要求模拟结果成功。模拟可能因余额、授权等条件不满足而失败。

该流程不会执行 ERC-20 授权、钱包签名或广播。项目中的 Rust 策略模块需要调用方显式调用；直接运行这个 YAML **不会自动执行完整的策略检查**。

## 跟踪已经广播的交易

这个功能走独立 JSON-RPC 传输，不需要 Binance Web3 凭据或钱包私钥。把哈希替换为已广播交易的完整哈希，即 `0x` 加 64 位十六进制字符：

```bash
export FLOW_BNB_RPC_URL='https://bsc-dataseed.bnbchain.org'

cargo run --locked -- watch-transaction \
  --tx-hash 0xYour64HexDigitTransactionHash \
  --chain-id 56 \
  --confirmations 3 \
  --timeout-ms 120000 \
  --report receipt-report.json \
  -v
```

命令先检查 RPC 链 ID，再循环读取回执并核对其所属区块，计算确认数。交易所在区块计为第 1 次确认；回执消失或区块不再匹配时，会重新等待。

| 参数 | 默认值 | 用途 |
| --- | --- | --- |
| `--chain-id` | `56` | 预期链 ID |
| `--confirmations` | `3` | 成功所需的确认数 |
| `--timeout-ms` | `120000` | 回执循环的总期限，毫秒 |
| `--interval-ms` | `1000` | 轮次之间的等待时间，毫秒 |
| `--max-iterations` | `120` | 最多轮询次数 |
| `--request-timeout-ms` | `10000` | 单步读取的请求预算，毫秒 |

初始链检查位于回执循环之前，受请求超时限制。总耗时可能包含这部分时间。

命令输出 JSON 报告，仅 `confirmed` 返回成功退出码。链上执行失败、超时、次数用尽或 RPC 错误均返回非零退出码；超时不代表交易已经失败或丢失。`--report` 保存同一份结果，成功或失败都可以留档，但拒绝覆盖已有文件。

`transaction_receipt.http.yml` 依赖专用适配器提供的 `tracking` 字段，应通过 `watch-transaction` 或 MCP 的 `watch_transaction` 使用。不能把它当作普通 Binance API Flow，用 `run` 命令直接执行。模板在编译时嵌入程序，修改后需要重新编译；运行时通过 CLI / MCP 参数调整轮询限额。

更多参数和回执校验语义见 [回执跟踪说明](docs/receipt-tracking.md)。

## 接入 Agent：MCP 服务

服务使用官方 Rust MCP SDK，通过 stdio 与 Agent 通信：

```bash
cargo run --locked --bin flow-bnb-mcp -- --root "$PWD"
```

这是一条供 MCP 客户端启动服务的命令。服务等待客户端协议请求；直接在终端启动后没有普通交互提示属于正常情况。

生成文件只能写入 `--root` 指定目录以内，不允许通过符号链接逃逸。

| 工具 | 用途 |
| --- | --- |
| `list_bnb_capabilities` | 列出模板、官方 API 路径及能力边界 |
| `generate_bnb_flow` | 生成经过编译的标准 YAML，可保存到工作目录 |
| `validate_bnb_flow` | 离线解析并编译 Agent 生成的 YAML |
| `evaluate_trade_policy` | 校验交易意图，返回机器可读的违规原因 |
| `watch_transaction` | 跟踪已广播交易，返回结构化回执报告 |
| `build_execution_plan` | 生成包含报价、策略、模拟、确认、签名与广播阶段的执行计划 |

默认策略限制为 BSC，金额上限 100 USD、滑点上限 50 bps（0.5%）、价格影响上限 100 bps（1%）。策略的执行模式还要求模拟成功和操作者明确确认；调用方可提供代币白名单。

`build_execution_plan` 只生成计划，不会替用户签名或发送交易。`watch_transaction` 的业务结果需要检查报告中的 `success` 和 `outcome`。

## YAML 常用字段怎么看

| 字段 | 含义 |
| --- | --- |
| `inputs` | 定义流程需要的输入，可设置默认值和敏感标记 |
| `literal` | 固定值，例如 `{ literal: 200 }` 就是数字 200 |
| `input` | 引用流程输入，例如 `{ input: ticker }` |
| `concat` | 将多段内容拼接起来，例如拼接请求 URL |
| `output` | 引用某个步骤导出的值 |
| `checks` | 校验 HTTP 状态或响应内容，不满足时步骤失败 |
| `exports` | 从步骤响应中提取值，供后续步骤引用 |
| `outputs` | 声明整个流程的最终输出 |
| `kind: repeat_until` | 重复执行内部步骤，直到条件满足或触发限制 |

例如，下面的表达式引用 `search-token` 步骤提取的合约地址：

```yaml
output:
  step: search-token
  name: contract_address
```

可以从 [股票代币查询模板](flows/rwa_discovery.http.yml) 开始阅读，先理解输入、请求、校验和导出，再看回执模板中的循环。

## 当前边界和后续工作

当前已经覆盖查询、交易准备与模拟、执行前策略复核、人工确认、开发钱包交接和回执跟踪。开发钱包通过本地节点的解锁账户提交交易，Flow 不加载私钥；MCP 没有直接批准或签名工具。

可以先运行无需个人钱包或真实资金的离线演示：

```sh
python3 scripts/demo_handoff.py
```

完整命令、人工确认流程、状态含义及配置见 [MCP 交易请求交接与本地开发钱包（中文）](docs/wallet-handoff.zh-CN.md)。演示使用真实进程和模拟 API/RPC，不包含真实签名、EVM 执行或大模型调用。

生产钱包、RFQ 适配、真实授权与兑换模拟，以及小额主网交易验收仍需完成。回执确认数依据 RPC 节点视图计算，不等同于绝对最终性。

开发计划见 [路线图](docs/roadmap.md)，已有验证记录见 [开发者体验报告](docs/developer-experience-report.md)。

## 本地验证

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets

for file in flows/*.http.yml; do
  cargo run --locked -- check "$file"
done
```

这些命令用于格式、静态检查、测试和模板编译。访问真实 Binance API 的效果，需要配置凭据并单独执行相应 Flow。

## 许可证

MIT，见 [LICENSE](LICENSE)。
