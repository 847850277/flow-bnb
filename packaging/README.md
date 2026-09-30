# Multi-client desktop distribution

The package contains compiled Flow binaries (MCP and engine together), templates,
a polling script and a dependency-free Node launcher. No Rust compilation, npm postinstall hook,
private key or credential is shipped. JSONC/TOML parsers are bundled from locked
maintainer dependencies. The installer never changes client tool-approval policy.

## Install and connect

After public release, download `install-flow-bnb.sh` and run it. The interactive
menu asks which clients to configure. On macOS `install-flow-bnb.command` provides
the same entry; Terminal/Gatekeeper may require the user's normal launch approval.
The script downloads a fixed-version HTTPS archive, verifies its embedded SHA-256,
prepares a private Node runtime if needed, then launches the menu. System `curl`,
`tar`, `shasum`/`sha256sum` are required, but Rust and global npm installs are not.
An archive beside the script is preferred, enabling offline package transfer;
first wallet dependency installation/login still needs network access.

```sh
sh install-flow-bnb.sh --client codex,cursor,vscode
sh install-flow-bnb.sh --client roo,continue --project /absolute/project
sh install-flow-bnb.sh --client generic
sh install-flow-bnb.sh --list
# Preview client edits only; Flow/runtime files may still be prepared.
sh install-flow-bnb.sh --client codex --dry-run
```

Use `--no-login` to configure clients without wallet access. Otherwise setup
prepares the pinned wallet CLI and opens official Binance pairing. If login fails,
client configuration is retained; reconnect in chat with `connect_bnb_wallet` and
poll `get_bnb_connection` every 3–5 seconds. Show the official URL and pairing code.
Keep the MCP client open while pairing. Login never creates a trading mandate.
The MCP handshake does not wait for wallet installation or authentication.

Since v0.2.0, explicit native execution requests start trading directly without a
Flow terminal confirmation or an `agentic-operator` process. Strategy mandates
are optional budgets. Quote and strategy-preview tools remain read-only. Keep
the MCP client running during execution so it can return the order and settlement.

WorkBuddy 5.0+ imports `flow-bnb-workbuddy.zip` using its documented stdio
`preAuth: "cli"` lifecycle (init/auth/status/unAuth, managed Node 22,
`authWaitForExit: true`). Claude Desktop on macOS imports `flow-bnb.mcpb` using its
embedded Node runtime (22+ required); after import, connect the wallet in chat.
These are independent alternatives to the universal script, not extra steps.

## Adapter coverage

| ID | Configuration location / shape |
| --- | --- |
| `codex` | `$CODEX_HOME/config.toml` or `~/.codex/config.toml`, TOML `mcp_servers` |
| `claude-code` | `~/.claude.json`, `mcpServers` (respects `CLAUDE_CONFIG_DIR`) |
| `claude-desktop` | macOS `~/Library/Application Support/Claude/claude_desktop_config.json` |
| `cursor` | `~/.cursor/mcp.json`; also exports an install deep link |
| `vscode`, `vscode-insiders` | Default user profile `Code[/ - Insiders]/User/mcp.json`, `servers` + `type: stdio` |
| `copilot` | `~/.copilot/mcp-config.json`, `mcpServers`, `type: local`, exposed tools list |
| `windsurf` | Legacy `~/.codeium/windsurf/mcp_config.json` |
| `devin` | `$XDG_CONFIG_HOME/devin/mcp_config.json` or `~/.config/devin/mcp_config.json` |
| `cline` | CLI `~/.cline/mcp.json`; IDE: supply its displayed config with `--config` |
| `gemini` | `~/.gemini/settings.json` |
| `kiro` | `~/.kiro/settings/mcp.json` |
| `qoder` | Qoder CLI `~/.qoder/settings.json` |
| `opencode` | `$XDG_CONFIG_HOME/opencode/opencode.json[c]`, or `OPENCODE_CONFIG`; `mcp`, command array, `environment` |
| `roo` | Selected project `.roo/mcp.json` |
| `continue` | Selected project `.continue/mcpServers/flow-bnb.yaml` (JSON-compatible YAML) |
| `generic` | Exports configurations for other local stdio clients, no direct registration |

VS Code's macOS profile base is `~/Library/Application Support`; Linux uses
`$XDG_CONFIG_HOME` or `~/.config`. A custom profile/portable installation can use
`--client vscode --config /absolute/profile/mcp.json`. Quit clients before editing
their config; project/enterprise policies can override global entries. Detection
means an existing configuration directory, not proof the application is installed.

Trae, Cherry Studio and other GUI clients: select `generic`, then import
`~/.local/share/flow-bnb/client-configs/generic.json` where JSON import is supported,
or copy its `command`, `args`, `env` into local stdio server settings. We do not
edit undocumented GUI databases or claim automatic registration for these apps.
Remote-only HTTP/SSE clients cannot directly launch this local stdio server.

## Preservation and updates

All clients use the same persistent Flow binary and workspace under
`~/.local/share/flow-bnb/` (override with absolute `FLOW_BNB_HOME`). Client entries
point directly at the durable native executable, not npm's cache or a temporary
installer. Node used by managed `baw` is also bound by its absolute launcher path.
Wallet state, policies, order locks and user-edited flows survive reinstall/logout.
Source-checkout settings are not silently migrated; reconnect must match the saved
wallet. Disconnect does not recall already submitted orders.

Update a connector by importing the new release bundle and reconnecting MCP, or
rerun the new universal installer for a registered client. When connected,
`get_bnb_connection` reports `execution_mode: "direct"` and
`confirmation_required: false` since v0.2.0. Historical `agentic-*.lock` and
`halted.json` files are preserved but no longer block execution. Startup never
drains old queued requests. Request deduplication and record-write synchronization
remain in place.

Since v0.3.0, the installer also verifies and stores `run-cycle.sh` beside the
versioned binary. The script uses that binary directly and only schedules cycle
commands; it does not implement price conditions. See the [linked-stock demo](../docs/linked-stock-cycle-demo.md)
for commands using the existing installed workspace.

Registration preserves unrelated settings and JSONC/TOML comments. Existing files
are backed up with private permissions before atomic replacement. Repeated setup
is idempotent. Receipts permit upgrading an unchanged installer-owned entry;
manual changes or unrelated same-name entries cause a conflict, never silent
replacement. Invalid/duplicate JSON keys, symlink config paths and concurrent
changes are rejected. Close clients to avoid their own in-memory settings writes.
No `autoApprove`, trust-all or global approval disabling is added.

## Maintainer build and acceptance

```sh
npm ci --prefix packaging --ignore-scripts --no-audit --no-fund
node --test packaging/desktop/*.test.mjs
cargo +1.90.0 build --locked --release --bin flow-bnb
mkdir -p dist/native/darwin-arm64
cp target/release/flow-bnb dist/native/darwin-arm64/flow-bnb
node packaging/build.mjs --local --output dist/preview
node packaging/acceptance.mjs dist/preview
```

Use the actual host key: `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`.
Output must be a new directory. Local script + sibling tarball are portable to the
same platform; the local WorkBuddy connector references the absolute preview
package directory and must remain there. Do not distribute a `--local` connector
as a public package. Local `.mcpb` contains only the host binary; public bundles
include all four binaries. macOS x64/arm64 bundle UI testing is separate.

Acceptance uses synthetic client homes, the real npm tarball and bundled parsers,
real MCP initialization/tool discovery/strategy generation, and the universal
shell installer. The official TypeScript MCP SDK validates the complete tool list,
compiles every output schema and checks sampled structured results and their text
fallbacks. It never registers into real clients or logs into a real wallet.
`tests/setup.rs` exercises wallet pairing and reconnect with a fixture backend,
including nonblocking MCP pairing, account mismatch and preservation of locks.

## Release boundaries

Release CI builds macOS arm64/x64, Linux x64 (Ubuntu 22.04 / glibc 2.35+) and Linux
arm64 (Ubuntu 24.04 / glibc 2.39+); Linux requires OpenSSL 3 runtime libraries.
Native Windows and musl/Alpine are unsupported. WSL-to-Windows client registration
is not implemented. A local build does not validate every host or client UI.

Workflow dispatch produces artifacts. A matching `v<Cargo version>` tag attaches
the assets to an existing release, preserving its draft/published state, or creates
a **draft** release when none exists. This supports creating the release and tag
together in the GitHub UI. Existing assets are never overwritten; use a new version
when their contents change. Cargo/npm versions must match; bump both when binary
contents change (installed version contents are immutable). Publish all assets:

- `flow-bnb-desktop-<version>.tgz`
- `install-flow-bnb.sh`, `install-flow-bnb.command`
- `flow-bnb-workbuddy.zip`, `flow-bnb.mcpb`
- `SHA256SUMS`

Public links use a fixed GitHub release version. There is no assumed npm registry
publication. Public repository/release publication and marketplace submission are
separate actions. [v0.3.0](https://github.com/847850277/flow-bnb/releases/tag/v0.3.0)
distributes all six assets. Packaged MCP acceptance includes generation, saving
and synthetic replay of the cross-asset cycle. Automated checks do not establish
UI acceptance in every supported client or a live-trade test of the new cycle.

## Primary configuration references

- [Codex](https://developers.openai.com/codex/mcp), [WorkBuddy](https://open.workbuddy.cn/docs/connector)
- [Claude Code](https://code.claude.com/docs/en/mcp), [Claude MCP bundles](https://github.com/anthropics/mcpb/blob/main/MANIFEST.md)
- [Cursor](https://cursor.com/docs/mcp/install-links), [VS Code](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)
- [Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers)
- [Devin / Windsurf](https://docs.devin.ai/desktop/cascade/mcp), [Cline](https://docs.cline.bot/mcp/mcp-overview)
- [Roo Code](https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo/), [Continue](https://docs.continue.dev/reference/continue-mcp)
- [Gemini](https://geminicli.com/docs/tools/mcp-server/), [Kiro](https://kiro.dev/docs/mcp/configuration/)
- [Qoder CLI](https://docs.qoder.com/cli/mcp-reference), [OpenCode](https://opencode.ai/docs/mcp-servers/)
