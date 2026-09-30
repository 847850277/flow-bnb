# 股票参考价偏离监控

场景：读取 AAPLon 的链上代币价和 API 参考价，计算带方向的差值，达到条件时生成候选交易意图。模板为 `flows/stock_spread_strategy.http.yml`，MCP 模板名为 `stock_spread_strategy`。

## 指标口径

```text
spread_bps = (tokenPrice - referencePrice) / referencePrice × 10000
```

正数表示高于参考价，负数表示低于参考价；100 bp = 1%。比较使用精确十进制整数与交叉相乘，输出中的 `spread_bps` 和 `spread_percent` 仅展示时截断到小数点后六位，触发判断不使用截断值。

[Binance RWA 官方文档](https://web3.binance.com/en/dev-docs/catalog/web3-wallet/api/rest-api/rwa-data)将 `referencePrice` 定义为由链上代币价换算的每股参考价。因此这里衡量的是两个 API 字段的偏离，不能视为相对独立美股行情的折溢价或套利收益。结果固定标注 `reference_price_basis: token_derived_per_share`。真正的标的市场折溢价还需要独立行情和代币对应股份数量的换算。

该价格接口只提供 `tokenPriceUpdatedAt`，未提供独立的参考价时间戳。结果中的 `reference_price_updated_at` 为 `null`；新鲜度检查仅针对代币价格，不能保证标的市场正在交易或参考价同步更新。

固定价格条件是“价格不高于 98 就买”；相对条件是“低于参考价至少 2% 就买”。后者在参考价为 100 时对应 98，在参考价为 110 时对应 107.8。本模板使用相对条件。

## 默认条件与输出

| 输入 | 默认值 | 含义 |
| --- | --- | --- |
| `stock_token` | AAPLon 的 BSC 合约 | 必须与响应中的合约及交易意图中的一侧代币一致 |
| `operator` | `lte` | 支持 `eq`、`gt`、`gte`、`lt`、`lte` |
| `threshold_bps` | `-100` | 低于参考价至少 1%，含恰好 1% 的边界 |
| `max_age_seconds` | `300` | 代币价格更新时间距求值时刻不超过 300 秒，允许设置 1～86400 秒 |
| `intent` | 6 USDT 换 AAPLon，滑点容忍 50 bp | 候选意图，仍受本地代币及数量限制 |

每次求值只做一次官方价格 GET 请求，再在本地计算和生成决策。未达到条件时，运行成功、`threshold_met=false`、`decision.triggered=false`。达到条件时，两者为 `true`；只读求值本身不排队、不下单，也不会启动常驻监控。

缺失价格、非正价格、格式错误、空或多行响应、链或合约不符、监控代币不在交易意图内、过期或未来时间戳均使求值失败，不产生交易决策。HTTP 200 的业务错误同样停止流程。

输出的 `spread` 包含原始价格、带符号的 bp 和百分比、`above_reference`／`at_reference`／`below_reference`、代币价格时间和年龄、阈值、比较符及指标口径，便于演示时说明触发原因。

## 只读运行

本机已有 Agentic 配置，且 CLI／MCP 进程环境已经配置 `BINANCE_WEB3_API_KEY` 和 `BINANCE_WEB3_SECRET_KEY` 后运行。密钥不写入 YAML，也不通过 MCP 参数或对话传入；CLI 不自动读取 `.env`。

```sh
# 默认：低于 API 参考价至少 1%，只读求值
cargo run --locked -- strategy-run flows/stock_spread_strategy.http.yml

# 高于 API 参考价至少 1% 时，产生卖出 0.01 AAPLon 的候选意图；仍只读
cargo run --locked -- strategy-run flows/stock_spread_strategy.http.yml \
  --input 'operator=gte' \
  --input 'threshold_bps=100' \
  --input 'intent={"from_token":"0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4","to_token":"0x55d398326f99059fF775485246999027B3197955","amount":"0.01","slippage_bps":50}'
```

## WorkBuddy 演示

可使用下面的自然语言请求：

> 生成 stock_spread_strategy 模板，保存为 strategies/apple-spread.http.yml。监控 AAPLon 链上价相对 API 参考价的偏离，低于至少 1% 时给出 6 USDT 的候选买入意图，价格超过 300 秒则停止。先校验并只读运行，展示两种价格、偏离百分比、更新时间、指标口径和是否达到条件。

对应 MCP 顺序为 `generate_bnb_flow` → `validate_bnb_flow` → `save_bnb_flow` → `run_bnb_strategy`。`list_bnb_capabilities` 中包含新模板和参数说明。

实际行情未触发也能展示完整结果，无需为演示临时放宽真实交易权限。已明确请求执行时，可使用现有 `request_bnb_strategy_execution` 入队；操作员确认后重新获取价格并求值，条件消失或数据失效会阻止提交。预授权流程也冻结相同 YAML 和输入，并沿用相同的执行前复查。实际交易继续经过原生钱包的账户、余额、审计、报价及滑点检查。

## 离线验证范围

测试使用临时配置和内存 HTTP 响应，不访问真实钱包、真实 API 或项目的私有状态目录。

| 测试数据 | 预期结果 |
| --- | --- |
| 参考价 100，代币价 99，`lte -100 bp` | 触发候选意图 |
| 参考价 100，代币价 99.000000000000000001，同一条件 | 不触发，不能被浮点舍入成边界 |
| 参考价 100，代币价 101，`gte 100 bp` | 触发高于参考价的条件 |
| 时间恰好在 300 秒边界／超出 1 毫秒 | 前者通过，后者停止 |
| 参考价为 0、错误合约、业务失败或缺字段 | 求值失败且无决策 |
| 首次达到条件，执行前新价格已不满足 | 复查拒绝提交 |

离线验证覆盖计算、策略编排、复查及 MCP 模板生成；真实 API 和 WorkBuddy 的界面演示需在本机凭据环境完成。
