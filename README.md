# Flow BNB

基于 `postman-flow` 的 BNB Chain 交易工作流工具，提供 Rust CLI 和 MCP 服务。

**交易意图 → 钱包与策略检查 → 报价 → Agentic Wallet 直接执行 → 订单跟踪 → 链上到账核对。**

- `flows/`：资产发现、钱包查询、报价与模拟、原生订单和回执跟踪模板。
- `src/`：Flow 适配器、交易规则、持久化请求队列和 MCP 工具。
- `examples/`：钱包配置及交易请求样例。

## 客户端安装

安装器把 **Flow、MCP 和钱包依赖**作为一个入口交付，无需 Rust、手写 MCP 路径或单独安装 `baw`。支持以下接入方式：

| 客户端 | 安装方式 |
| --- | --- |
| WorkBuddy 5.0+ | 导入 `flow-bnb-workbuddy.zip`，自动准备依赖并登录 |
| Claude Desktop（macOS） | 导入 `flow-bnb.mcpb`；也支持通用安装器 |
| Codex、Claude Code、Cursor、VS Code / Copilot、Copilot CLI | 安装器选择客户端，自动合并配置 |
| Windsurf（旧配置路径）、Devin Desktop、Cline CLI、Gemini CLI、Kiro、Qoder CLI、OpenCode | 同一安装器自动合并配置 |
| Roo Code、Continue | 同一安装器，指定项目目录 |
| Cline IDE | 在其 MCP 设置中取得配置路径，用 `--client cline --config <路径>` 安装 |
| Trae、Cherry Studio、其他支持本地 stdio 的客户端 | 导入生成的通用配置，或在 MCP 设置中填写生成的命令、参数、环境变量 |

从 Release 下载 `install-flow-bnb.sh`，运行后选择客户端即可；macOS 也提供 `.command` 文件。没有可用 Node/npm 时会自动准备私有运行环境。命令行可直接选择多个客户端：

```sh
sh install-flow-bnb.sh --client codex,cursor,vscode
# 项目型客户端
sh install-flow-bnb.sh --client roo,continue --project /绝对路径/项目
# 其他客户端：生成可导入配置
sh install-flow-bnb.sh --client generic
```

首次按提示在币安 App 完成钱包配对，随后重启客户端、启用 MCP。也可以加 `--no-login` 先安装，再在对话中说“连接 Flow BNB 钱包”；Agent 使用 `connect_bnb_wallet` / `get_bnb_connection` 展示官方链接、配对码及进度。登录不创建交易授权，客户端自身的工具审批设置保持原样。

配置合并会保留其他 MCP、用户设置和注释，并备份原文件；重复安装不会重复添加。升级只更新安装器记录且未被用户改动的 Flow 条目，遇到同名冲突会停止。所有客户端默认共用 `~/.local/share/flow-bnb/` 中的钱包配置、策略和交易记录；已有源码工作区不会被自动迁移。

**安装包已在 [GitHub Releases](https://github.com/847850277/flow-bnb/releases) 提供，四个平台构建和打包验收已通过，各客户端界面的逐一验收仍待完成。** 支持 macOS arm64/x64、Linux glibc x64/arm64；尚不支持 Windows 原生、Alpine 或只接受远程 HTTP/SSE 的客户端。构建本地预览和完整兼容性说明见 [安装包说明](packaging/README.md)。

## 源码开发与本地运行

需要 Rust（最低 1.90，仓库工具链指定 1.97）和 macOS/Linux。首次在仓库根目录运行：

```sh
cargo run --locked -- setup
```

Flow 自动准备 `baw`、引导币安 App 扫码登录、绑定 BSC 地址并生成配置。无需单独安装 `baw` 或填写签名器路径；已有登录、交易限额和订单状态会保留。新配置默认白名单为 USDT / AAPLon，单笔卖出上限分别为 6 / 0.01，滑点上限 0.5%，保存在 `.flow-bnb/agentic.json`。

依赖安装在配置目录下的 `managed/`，不修改全局 npm。固定使用 `baw 1.10.0`；优先复用 Node 18+，缺少可用 Node/npm 时自动下载经固定 SHA-256 校验的 Node 22.23.3。自动安装支持 macOS/Linux arm64/x64（Linux 需兼容 glibc），下载需要系统 `curl`、`tar` 及 Node/npm 官方站点网络访问。不会静默升级已有后端。

遇到问题运行 `cargo run --locked -- doctor`；登录失效或安装中断可重跑 `setup`。`setup --no-open` 只显示登录链接，`setup --no-login` 准备依赖但不发起新登录。手机确认最长等待五分钟，期间保持终端开启；无需提供私钥或助记词。

只读准备示例（请求卖出 0.01 AAPLon，需有相应余额）：

```sh
cargo run --locked -- agentic-trade \
  --request examples/agentic-sell.json \
  --report ".flow-bnb/preparation-$(date +%Y%m%d-%H%M%S).json"
```

加 `--execute` 会直接下单，无需输入 `CONFIRM`，也无需操作员终端。币安明确返回审计无数据或不支持时，准备结果为 `ready`，`token_audit.status` 保持 `unavailable` 并附带提示，不增加确认步骤，也不把缺少数据当作安全证明。已知风险、接口错误、响应格式错误和余额不足仍会停止本次准备。

`amount` 使用人类可读单位，例如 `"0.01"`；`slippage_bps: 50` 表示 0.5%。恢复已有报告的只读跟踪使用 `agentic-track --report <原报告路径>`，不会重新下单。

## MCP 接入

`setup` 会生成 `.flow-bnb/mcp.json`，把其中的 `flow-bnb` 条目添加到 MCP 客户端即可，无需手填路径。CLI 与 MCP 使用同一个 Flow 可执行文件（`flow-bnb mcp`）；配置还携带 Node 路径，适用于桌面客户端的精简环境。移动或重建安装位置后重新运行 `setup` 生成配置。

钱包连接后，简单买卖只需说明交易意图：询价调用只读工具；明确请求买卖则调用执行工具，直接执行并查询结果。无需先写 DSL、创建策略授权或运行 `agentic-operator`。执行期间保持 MCP 进程运行，客户端自身的工具权限提示由客户端控制。

| MCP 工具 | 用途 |
| --- | --- |
| `connect_bnb_wallet` / `get_bnb_connection` | 准备依赖、官方钱包配对及只读状态查询 |
| `prepare_agentic_trade` | 只读检查与报价；审计无数据作为提示返回 |
| `request_agentic_execution` | **直接启动真实交易**；接收 `request_id` 和 `intent`，网络重试复用相同 ID 与参数 |
| `get_agentic_execution` | 用返回的 `intent_id` 查询执行结果 |
| `cancel_agentic_execution` | 取消尚未开始的旧队列请求；执行工具通常立即启动，不能撤回已提交订单 |
| `refresh_agentic_execution` | 恢复已有订单的只读核对 |
| `inspect_agentic_order` | 核对其他入口已提交的订单 |

一次执行调用返回 `executing` 和 `intent_id`，随后用 `get_agentic_execution` 查询到账结果。同一请求 ID 永不重复下单；提交结果未知时回查已有订单，不能换 ID 重放。报价调用永远不会下单。

## 自定义策略

在 WorkBuddy 中描述条件后，AI 可参考 `generate_bnb_flow(template="stock_strategy")` 编写 YAML，再依次调用 `validate_bnb_flow`、`save_bnb_flow`、`run_bnb_strategy`。`read_bnb_flow` 返回源码和 SHA-256；覆盖文件必须提供旧哈希，避免覆盖未审阅的修改。编译通过不代表策略收益或执行安全得到保证，运行时还会检查数据源与本地限额。

显式请求执行时调用 `request_bnb_strategy_execution`，传入文件路径、`expected_sha256`、输入和稳定的 `request_id`。条件成立即在后台执行，返回的 `intent_id` 用于查询及回查。已开始的同 ID 重试返回原状态；条件不成立时不创建订单，再次调用会重新求值。执行记录冻结 YAML 和输入，提交前复查条件；条件失效、意图改变或检查失败会停止本次提交。

样例 `flows/stock_strategy.http.yml`：用 6 USDT 询价，报价至少获得 0.02 AAPLon 才触发。可修改输入、条件分支和有限次数循环。CLI 与 MCP 使用相同执行器：

```sh
# 只读求值，不下单
cargo run --locked -- strategy-run flows/stock_strategy.http.yml \
  --input 'min_receive="0.02"'
# 加 --execute <稳定请求ID> 在条件成立时直接下单，无需终端确认
```

股票场景使用 `generate_bnb_flow(template="stock_spread_strategy")`：读取 AAPLon 的链上价和 API 参考价，计算 `(链上价 - 参考价) / 参考价 × 10000`，默认低于参考价至少 100 bp（1%）时产生 6 USDT 的候选买入意图。正值表示高于参考价，负值表示低于参考价；参数支持改为溢价条件及卖出意图。计算使用精确十进制，缺价、零价、过期价格及代币不匹配均停止求值。此场景需要 Web3 API 凭据，执行前复查沿用同一冻结策略。

官方 `referencePrice` 是由链上价格换算的每股参考价，因此该指标表示 API 字段间偏离；独立标的市场的真实折溢价还需要外部行情与份额换算。输出标明参考价口径，更新时间检查仅针对 `tokenPriceUpdatedAt`。完整的只读命令、边界说明和 WorkBuddy 演示提示见 [股票参考价偏离监控](docs/stock-spread-demo.md)。

策略使用标准 Flow YAML。内置本地端点 `https://flow-bnb.invalid/strategy/quote` 接收交易意图、返回原生报价；`compare` 接收十进制字符串 `left` / `right` 和 `operator`（eq/gt/gte/lt/lte）；`rwa-spread` 接收 RWA 价格数组、监控代币、交易意图、比较符、带符号的 bp 阈值和最大价格年龄；`decision` 接收 `triggered` 和 `intent`。这些 POST 由本地适配器处理。也允许官方 Web3 的 RWA 平台、搜索、价格和钱包余额 GET 查询，相关步骤需要 API Key。其他网络地址、文件请求体、认证注入及直接下单操作会被拒绝。

每次最多 30 秒、32 次请求、一个交易决策；YAML 上限 64 KiB、输入上限 16 KiB。试运行使用实时只读数据，不是历史回测，也不会启动常驻监控或定时任务。

## 可选的策略预算

直接交易无需预授权。如果需要对某个固定策略设置累计卖出额度、单数、间隔和到期时间，可运行一次预算授权命令，固定源码、输入、钱包、交易对、单笔数量和滑点。例如：

```sh
cargo run --locked -- strategy-authorize flows/stock_strategy.http.yml \
  --id apple-small --max-orders 3 --max-total-sell-amount 18 \
  --valid-for-minutes 60 --cooldown-seconds 300
```

这会保存后续执行使用的策略预算，但不立即下单或启动定时任务。策略须始终输出 `decision`（条件不满足时 `triggered=false`），首次只读检查成功才创建授权。上例沿用样例的每单 6 USDT，最多 3 单、累计最多 18 USDT、至少间隔 300 秒、1 小时后到期。额度按卖出代币数量计，BNB 手续费另计。

接入 MCP 后，Agent 用 `get_bnb_strategy_authorization` 查找授权，调用 `execute_bnb_authorized_strategy`（`authorization_id`、`request_id`）启动一次自动求值，再用 `get_bnb_authorized_execution` 查询结果。无需另开操作员终端；执行期间保持 MCP 进程运行。也可直接运行：

```sh
cargo run --locked -- strategy-auto --authorization-id apple-small --request-id apple-check-001
# 停止未来提交；不能撤回已经开始提交的订单
cargo run --locked -- strategy-revoke --id apple-small
```

预算模式下，同一 `request_id` 永不重复执行，包括条件未触发的检查；下一次独立检查使用新 ID。中断、提交结果不明或到账差额记录在当前请求中，不再锁住整个钱包或永久停止后续独立请求。已预留额度仍计入预算，不因结果未知自动退回。`refresh_bnb_authorized_execution` 只回查已有订单。MCP 可撤销预算授权，不能创建或扩大它；策略及额度变化需新建授权。每个授权最多保留 1024 次求值记录，此模式不是常驻调度器。

## 当前边界

原生执行面向 BSC 的 ERC-20 兑换，保留账户、链、余额、代币数量和滑点检查；没有继承旧 Web3 后端的美元额度、价格冲击或独立模拟保证。非内置可信目标会查询代币审计：明确无数据只提示；已知风险、异常税率或响应异常会停止本次准备。Agentic Wallet 使用自己的登录会话，原生订单无需 Web3 API Key；旧 Web3 API 数据流程需要单独配置认证。

`completed` 表示成交与数量核对一致；`settled_with_discrepancy` 表示已成交但存在数量差额。差额保留在结果中，不自动补卖或重试，也不锁住后续独立订单。同一请求的去重记录和文件写入同步保留，避免重复下单和记录损坏。钱包服务自身的签名及风控仍由其控制。

从 **v0.2.0** 起使用上述直接执行流程；已安装的旧版需更新后才会生效。升级不自动执行旧队列，也不删除历史订单或旧锁文件；旧 `agentic-*.lock` 和策略 `halted.json` 不再作为执行门槛。旧 `strategy-run --enqueue` 保持仅保存请求；新的直接执行参数为 `--execute <request-id>`。`agentic-operator --intent-id` 仅作为明确执行某条旧请求的兼容入口，已移除 `--watch`。旧 Web3 签名器协议中的 `confirmation_id` 保留为交易绑定摘要，不再要求手工输入，也不表示用户曾逐笔确认。

## 开发检查

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

CI 还会编译检查所有 `flows/*.http.yml`。许可证：MIT。
