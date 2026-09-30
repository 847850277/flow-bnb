# WorkBuddy 演示：监控英伟达，买入苹果，再按条件退出

这是用户自定义的跨资产联动规则，不表示苹果是英伟达的影子股，也不预设两者之间存在稳定的价格关系。

## 一份 YAML 表达完整策略

源文件：[linked_stock_cycle.http.yml](../flows/linked_stock_cycle.http.yml)。

1. 首次运行，以固定 **5 USDT → NVDAon** 的只读报价记录启动基准。
2. 此后使用相同报价参数；NVDAon 的报价隐含价格相对启动基准下跌至少 **2%** 时，以 **5 USDT** 买入 AAPLon。
3. 等待订单与链上转账核对，保存本次实际花费和实际到账数量。
4. 仅对本次收到的 AAPLon 数量询价。当预计换回的 USDT 达到本次实际花费的 **102%** 时，提交一笔卖出。
5. 核对卖出回执并结束，不自动重新开仓，也不自动补卖余量。

第 4 步等价于：这笔数量的可卖报价均价比实际买入均价高至少 2%。它是报价触发条件，不保证成交后净赚 2%；最终实际到账、剩余数量和不含 gas 的现金差额单独报告。

这里的 NVDAon 涨跌幅来自固定金额报价的隐含价格，包含路由与报价成本影响；不是美股日内涨跌幅，也不是独立股票市场行情。基准在启动时固定，重启同一轮不重新取基准。此流程不需要 Web3 API Key。

合约来自 [Ondo 官方代币清单](https://github.com/ondoprotocol/ondo-global-markets-token-list/blob/main/tokenlist.json)，BSC（chainId 56）：

| 用途 | 代币 | 合约 |
| --- | --- | --- |
| 观察信号 | NVDAon | `0xa9ee28c80f960b889dfbd1902055218cba016f75` |
| 买入和卖出 | AAPLon | `0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4` |

观察 NVDAon 使用临时的只读报价配置，不将其加入实际交易白名单。真实交易继续使用现有钱包配置；5 USDT 买到的 AAPLon 可能超过新安装默认的 0.01 卖出上限，执行器在开仓前会检查预计持仓能否在该上限内退出。数量和限额须与用户实际计划一致，不自动扩大限额或拆单。

```mermaid
flowchart LR
    A[记录 NVDAon 启动基准] --> B[等待跌幅达到 2%]
    B --> C[买入 5 USDT 的 AAPLon]
    C --> D[记录实际成本与到账数量]
    D --> E[等待可卖报价上涨 2%]
    E --> F[卖出本次到账数量]
    F --> G[核对结果并结束]
```

## 分工

- **WorkBuddy**：将需求变成 YAML，校验、保存并解释结果。
- **YAML**：定义观察对象、买入对象、数量、百分比条件及分支。
- **Flow 引擎**：保存基准和阶段、承接买入回执、为两笔订单固定请求 ID、核对结果。
- **外层脚本**：每隔一段时间调用一次命令，看到结束状态就退出；不计算涨跌幅、不决定买卖。

每次调用最多推进一笔交易，不在 MCP 内创建常驻后台监控。CLI 脚本进程需保持运行；同一 `run_id` 可在中断后继续。成交结果未知时不会换 ID 下新单。已知成交差额保留在回执中，后续退出使用实际到账数量，不补买差额。

## 先在 WorkBuddy 创建并验证

以下 MCP 工具从 v0.3.0 起提供。录制前导入 [v0.3.0 WorkBuddy 连接器](https://github.com/847850277/flow-bnb/releases/download/v0.3.0/flow-bnb-workbuddy.zip)，重启或重新连接 MCP；已连接的钱包配置和历史记录保留。

发送：

```text
用 Flow BNB MCP 为我创建一份跨资产交易流程，保存为 strategies/nvda-apple.http.yml。

监控 NVDAon：以策略启动时的固定金额报价隐含价格为基准，下跌 2% 时买入 5 USDT 的 AAPLon。买入后按链上实际到账数量建立这笔持仓，等这笔数量的可卖报价比实际买入成本高 2%，卖出并结束这一轮。

先用 generate_bnb_flow(template="linked_stock_cycle") 取得模板，按上述需求核对、校验并保存。展示 YAML 的主要参数、条件和阶段，再画一张流程图。保留源码哈希。先不要真实交易，不要把判断逻辑写进 Python 或 Shell，不要修改技能文件。
```

接着发送：

```text
用 replay_bnb_cycle 运行刚才保存的 YAML，带上源码哈希。

这是模拟演示：使用工具内置的合成报价和合成回执，不是实时行情或历史回测。在画面中保留“模拟”标识。

按时间顺序展示：建立基准、未触发、英伟达信号触发买入苹果、持仓期间未达到退出条件、达到条件卖出、再次调用不产生新订单。每一步都显示工具返回的阶段、判断依据和执行/跳过的节点；不要编造成交哈希或声称发生了链上交易。
```

默认模板的回放应生成两笔**模拟订单**，结束后的重复调用不会增加订单数。修改阈值会改变回放结果，不保证任意修改后都触发。

## 安装包用户直接运行

无需下载源码或安装 Rust。MCP 保存的策略位于 `~/.local/share/flow-bnb/workspace/`；轮询脚本与二进制一起安装到对应版本目录。

macOS Apple 芯片示例，先进入同一个工作区并做模拟验证：

```sh
flow_data="${FLOW_BNB_HOME:-$HOME/.local/share/flow-bnb}"
flow_version="$flow_data/versions/0.3.0-darwin-arm64"
cd "$flow_data/workspace"
"$flow_version/flow-bnb" cycle-replay strategies/nvda-apple.http.yml
```

Intel Mac 将 `darwin-arm64` 改为 `darwin-x64`；Linux 对应 `linux-arm64` 或 `linux-x64`。

用户确定实盘参数后运行（会按条件真实买卖）：

```sh
bash "$flow_version/run-cycle.sh" strategies/nvda-apple.http.yml nvda-apple-demo-01 --execute
```

脚本自动使用同目录二进制，钱包配置默认取当前工作区 `.flow-bnb/agentic.json`。首次调用记录基准；省略 `--execute` 时只轮询预览。

## 源码用户通过命令循环执行

源码构建：

```sh
cargo build --locked
```

无需钱包的模拟验证：

```sh
target/debug/flow-bnb cycle-replay flows/linked_stock_cycle.http.yml
```

真实行情的单次预览（不加 `--execute` 不提交订单）：

```sh
target/debug/flow-bnb cycle-step strategies/nvda-apple.http.yml \
  --run-id nvda-apple-demo-01 --config /path/to/agentic.json
```

使用与 MCP 相同的策略文件和钱包配置。首次调用记录基准；后续预览返回条件是否满足。可以先预览，再对同一轮启用执行，不必重新建立基准。

**用户确定实盘参数后**，以下命令会按条件启动真实买入和卖出：

```sh
FLOW_BNB_BIN="$PWD/target/debug/flow-bnb" FLOW_BNB_POLL_SECONDS=10 \
  bash scripts/run-cycle.sh strategies/nvda-apple.http.yml nvda-apple-demo-01 \
  --config /path/to/agentic.json --execute
```

脚本内容见 [run-cycle.sh](../scripts/run-cycle.sh)。它只调用 `cycle-step`、读取 `cycle-status` 并等待，没有买卖条件代码；循环阶段无需调用大模型。

查看本地结果：

```sh
target/debug/flow-bnb cycle-status --run-id nvda-apple-demo-01 \
  --config /path/to/agentic.json
```

运行状态在钱包配置的 `state_dir/strategy-cycles/` 中。活跃轮次的 YAML、输入和钱包配置保持一致；同一轮恢复时复用同一 ID。`Ctrl+C` 停止循环，不会撤回已提交订单，也不会自动卖掉持仓。

## 录制安排

主线控制在约 3 分钟：自然语言需求与参数 30 秒，YAML 和流程图 40 秒，循环运行与阶段变化 80 秒，回执与结束状态 30 秒。

先用模拟回放确认完整流程，再选择录制模拟演示或真实行情。实盘的两个 2% 条件可能长时间不触发；可剪掉等待并保留时间标记。模拟画面与真实成交应分别标注，不把两种结果拼成同一笔策略交易。

本场景展示可编辑的跨资产条件、持仓状态承接和完整退出流程。它没有证明两个资产存在预测关系，也未证明比官方工具更快。
