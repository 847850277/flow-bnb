# Flow BNB

**自然语言驱动的链上交易流程引擎 · Natural-language-driven transaction workflow engine**

在 WorkBuddy 等 MCP 客户端中说出交易需求，Flow BNB 完成报价、执行、订单跟踪和链上到账核对。支持股票代币买卖，以及通过 Flow DSL 定义报价阈值、链上价对 API 参考价的偏离等条件策略。底层基于 `postman-flow`，同时提供 Rust CLI。

**说出交易需求 → 自动检查与报价 → 执行 → 返回到账结果。**

当前版本：[v0.2.0](https://github.com/847850277/flow-bnb/releases/tag/v0.2.0) · [WorkBuddy 下载](https://github.com/847850277/flow-bnb/releases/download/v0.2.0/flow-bnb-workbuddy.zip) · [Claude Desktop 下载](https://github.com/847850277/flow-bnb/releases/download/v0.2.0/flow-bnb.mcpb)

## 快速开始

1. **安装**：在 WorkBuddy 5.0+ 中导入上面的连接器 ZIP，启用 Flow BNB。
2. **连接钱包**：首次按提示在币安 App 完成配对；已有连接可直接使用。未连接时，在对话中说“连接 Flow BNB 钱包”。
3. **说出需求**：询价、买卖或描述策略条件，由 Agent 调用工具完成。

| 你可以这样说 | 工具行为 |
| --- | --- |
| “查一下 5 USDT 能买多少 AAPLon” | 只读检查和报价 |
| “用 5 USDT 买入 AAPLon，滑点 0.5%” | 直接启动一次买入并查询到账结果 |
| “卖出 0.01 AAPLon 换成 USDT，滑点 0.5%” | 直接启动一次卖出并查询到账结果 |
| “写一个 AAPLon 链上价低于 API 参考价 1% 的买入策略，先试运行” | 生成策略并只读求值，不下单、不启动持续监控 |

执行期间保持客户端和 MCP 运行。钱包配对及客户端自身的工具权限由对应产品处理。

## v0.2.0 简化了什么

- **直接执行**：明确请求买卖后，MCP 直接启动交易；去掉逐笔 `CONFIRM` 和另开操作员终端的步骤。
- **不再永久锁住钱包**：到账差额和提交结果未知保留在订单中，不再拦住后续独立请求。
- **审计无数据只提示**：币安明确返回无数据或不支持时，不增加确认步骤。
- **简单买卖无需写代码或 DSL**：无需先创建策略授权、生成工作流或构建执行计划。

报价和策略预览仍然只读。执行工具会启动真实交易；同一请求的防重复下单、账户与余额检查、滑点检查、策略条件复查和到账核对继续保留。

## 更新已有安装

下载新版连接器，更新导入后重启或重新连接 MCP。使用通用安装器的客户端重新运行新版安装器即可。

钱包配置、历史订单和用户策略会保留。旧队列不会自动执行，旧锁文件无需手动删除。连接后让 Agent“查看 Flow BNB 连接状态”，新版返回 `execution_mode: "direct"`。

## 其他客户端

| 客户端 | 安装方式 |
| --- | --- |
| Claude Desktop（macOS） | 导入 [flow-bnb.mcpb](https://github.com/847850277/flow-bnb/releases/download/v0.2.0/flow-bnb.mcpb) |
| Codex、Claude Code、Cursor、VS Code / Copilot 等 | 运行通用安装器，选择客户端 |
| 其他本地 stdio MCP 客户端 | 安装器选择 `generic`，导入生成的配置 |

通用安装器：[install-flow-bnb.sh](https://github.com/847850277/flow-bnb/releases/download/v0.2.0/install-flow-bnb.sh) · [macOS .command](https://github.com/847850277/flow-bnb/releases/download/v0.2.0/install-flow-bnb.command)。无需 Rust 或单独安装钱包依赖。

```sh
sh install-flow-bnb.sh --client cursor
```

支持 macOS arm64/x64、Linux glibc arm64/x64；尚不支持 Windows 原生、Alpine 或只接受远程 HTTP/SSE 的客户端。完整客户端列表及构建方式见 [安装包说明](packaging/README.md)。

## 条件策略

在对话中描述条件，Agent 可生成、保存和运行 Flow DSL。你也可以编辑 YAML，组合数据查询、条件分支和有限次数循环。明确请求执行时，条件成立才提交订单，提交前会再次求值。

| 模板 | 场景 |
| --- | --- |
| [stock_strategy](flows/stock_strategy.http.yml) | 报价至少获得指定数量的代币时产生交易意图 |
| [stock_spread_strategy](flows/stock_spread_strategy.http.yml) | 链上价对 API 参考价的偏离达到指定 bp 阈值时产生交易意图 |

参考价场景需要 Web3 API 凭据；官方 `referencePrice` 是由代币价格换算的每股参考价，不能当作独立股票市场行情。计算口径和演示见 [股票参考价偏离监控](docs/stock-spread-demo.md)。

每次调用只做一次有时限的策略求值，不会自动开启持续监控或定时交易。DSL、CLI 和可选策略预算的详细用法见 [开发与进阶使用](docs/development.md)。

## 交易结果与范围

原生交易使用 Binance Agentic Wallet，在 BSC 上兑换 ERC-20 代币，无需 Web3 API Key。新安装默认配置 USDT / AAPLon，单笔卖出上限分别为 6 USDT / 0.01 AAPLon，滑点上限为 0.5%；已有配置保留原设置。

`completed` 表示成交与数量核对一致；`settled_with_discrepancy` 表示已成交但存在数量差额。结果未知时查询已有订单，不自动重试或补卖。缺少审计数据不代表代币安全；已知风险、响应异常及余额不足仍会停止本次准备。钱包服务自身的签名和风控继续生效。

## 开发

需要 Rust 1.90+ 和 macOS/Linux；仓库默认工具链为 1.97。源码运行：

```sh
cargo run --locked -- setup
cargo run --locked -- mcp
```

- [开发与进阶使用](docs/development.md)：源码配置、MCP 工具、DSL 和可选策略预算。
- [flows/](flows/)：工作流模板；[examples/](examples/)：配置与请求样例。
- [安装包说明](packaging/README.md)：客户端适配、打包和发布流程。

v0.2.0 已通过 112 项 Rust 测试、7 项安装器测试、10 个 Flow 模板编译、四个平台构建及打包验收；公开下载和校验值已验证。各客户端界面的逐一验收仍待完成。

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

许可证：[MIT](LICENSE)。
