# Flow BNB

基于 `postman-flow` 的 BNB Chain 交易工作流工具，提供 Rust CLI 和 MCP 服务。

**交易意图 → 钱包与策略检查 → 报价 → 操作员确认 → Agentic Wallet 执行 → 订单跟踪 → 链上到账核对。**

- `flows/`：资产发现、钱包查询、报价与模拟、原生订单和回执跟踪模板。
- `src/`：Flow 适配器、交易规则、持久化请求队列和 MCP 工具。
- `examples/`：钱包配置及交易请求样例。

## 快速开始

需要 Rust（最低 1.90，仓库工具链指定 1.97）。原生交易还需要 macOS/Linux 终端，以及已安装并登录的 Binance Agentic Wallet CLI `baw`。

在仓库根目录执行：

```sh
cargo build --locked --bins
mkdir -p .flow-bnb
cp examples/agentic-config.json .flow-bnb/agentic.json
```

编辑 `.flow-bnb/agentic.json`：填写 `baw` 的绝对路径、自己的 Agentic Wallet 地址和私有 `state_dir` 绝对路径，核对代币白名单、单笔数量和滑点限制。状态目录会创建为 0700；配置与钱包会话保留在本机。

只读准备示例（请求卖出 0.01 AAPLon，需有相应余额）：

```sh
cargo run --locked -- agentic-trade \
  --request examples/agentic-sell.json \
  --report ".flow-bnb/preparation-$(date +%Y%m%d-%H%M%S).json"
```

加 `--execute` 才进入真实执行流程，并要求在终端输入 `CONFIRM`。`amount` 使用人类可读单位，例如 `"0.01"`；`slippage_bps: 50` 表示 0.5%。恢复已有报告的只读跟踪使用 `agentic-track --report <原报告路径>`，不会重新下单。

## MCP 接入

将以下配置中的路径替换为本机绝对路径，添加到支持 stdio 的 MCP 客户端：

```json
{
  "mcpServers": {
    "flow-bnb": {
      "command": "/absolute/path/flow-bnb/target/debug/flow-bnb-mcp",
      "args": ["--root", "/absolute/path/flow-bnb"],
      "env": {
        "FLOW_BNB_AGENTIC_CONFIG": "/absolute/path/flow-bnb/.flow-bnb/agentic.json"
      }
    }
  }
}
```

另开操作员终端，使用同一份配置持续接收交易请求：

```sh
cargo run --locked -- agentic-operator --watch
```

| MCP 工具 | 用途 |
| --- | --- |
| `prepare_agentic_trade` | 只读检查与报价 |
| `request_agentic_execution` | 交易排队；接收 `request_id` 和 `intent`，重试同一请求必须复用相同 ID 与参数 |
| `get_agentic_execution` | 用返回的 `intent_id` 查询执行结果 |
| `cancel_agentic_execution` | 取消尚未被操作员领取的请求 |
| `refresh_agentic_execution` | 恢复已有订单的只读核对 |
| `inspect_agentic_order` | 核对其他入口已提交的订单 |

每笔交易仍在操作员终端确认；MCP 排队不等于授权。当前提供本地配置方式，WorkBuddy 实际客户端验收及公开地址一键安装包尚未完成。

## 当前边界

原生执行面向 BSC 的 ERC-20 兑换，检查账户、链、余额、代币数量和滑点等规则；没有继承旧 Web3 后端的美元额度、价格冲击或独立模拟保证。非内置可信目标需要有效的代币审计结果。Agentic Wallet 使用自己的登录会话，原生订单无需 Web3 API Key；旧 Web3 API 数据流程需要单独配置认证。

`completed` 表示成交与数量核对一致；`settled_with_discrepancy` 表示已成交但存在数量差额，需要复核。提交结果未知或有差额时保留提交锁，不自动重试或补卖；不要通过新请求 ID 或删除锁绕过检查。

## 开发检查

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

CI 还会编译检查所有 `flows/*.http.yml`。许可证：MIT。
