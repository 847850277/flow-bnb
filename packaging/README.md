# Desktop / WorkBuddy distribution

The connector uses WorkBuddy's documented **stdio MCP + `preAuth: "cli"`**
flow (WorkBuddy 5.0+). WorkBuddy supplies Node 22. The versioned npm tarball
contains Flow's compiled executables and templates; there are no npm dependencies,
postinstall scripts, Rust builds or wallet credentials in that tarball.

## User experience

Install/import the connector → init prepares Flow and `baw` → login shows the
official Binance URL and pairing code → the user confirms in the Binance App →
WorkBuddy starts MCP. `authWaitForExit` keeps the verification process alive.
Status checks are bounded/read-only; disconnect calls `baw auth signout`.
No onboarding command creates an automatic trading mandate or submits an order.

Data lives in `~/.local/share/flow-bnb/` (`FLOW_BNB_HOME` may select an absolute
alternative). `workspace/flows/` is editable. Configuration, policies, mandates,
order locks and reports remain outside npm's cache and survive reinstall/logout.
The wallet's credentials remain managed by the official wallet CLI. Reconnecting
must use the same wallet as the saved configuration. Source-checkout settings are
not silently migrated. Disconnect does not recall orders already submitted.

## Local acceptance (maintainers only)

```sh
cargo +1.90.0 build --locked --release --bin flow-bnb
mkdir -p dist/native/darwin-arm64
cp target/release/flow-bnb dist/native/darwin-arm64/flow-bnb
node packaging/build.mjs --local
node packaging/acceptance.mjs dist/release
```

Use the host platform directory (`darwin-arm64`, `darwin-x64`, `linux-x64` or
`linux-arm64`). The output directory must be new; use `--output <new-directory>`
for another build. `dist/release/flow-bnb-workbuddy.zip` is the local connector
preview. Its generated commands reference the absolute `dist/release/package/`
path: keep that directory in place while importing/testing the preview. It is not
portable or suitable for public distribution. The acceptance script uses an
isolated directory and real MCP, without wallet login or network trading APIs.
Wallet onboarding is tested separately with fixtures in `tests/setup.rs`.

## Public release

`.github/workflows/release.yml` builds macOS arm64/x64, Linux x64 (Ubuntu 22.04 /
glibc 2.35+) and Linux arm64 (Ubuntu 24.04 / glibc 2.39+); Linux requires OpenSSL 3
runtime libraries. Windows and musl/Alpine are not supported. Other platforms
need CI and actual host acceptance; a local arm64 build is not their validation.

A workflow dispatch only produces downloadable artifacts. A `v<Cargo version>`
tag also creates a **draft** GitHub release. The npm package version must match
Cargo. Bump both versions for a changed release; installed version contents are
immutable and a hash mismatch stops launch. Do not reuse an existing version.

The release contains `flow-bnb-desktop-<version>.tgz`, `flow-bnb-workbuddy.zip`
and `SHA256SUMS`. Publish the draft and both assets together, then distribute the
connector through WorkBuddy's supported import/submission flow. The connector
uses the fixed HTTPS GitHub release URL, not an unpublished npm registry name or
`latest`. No npm publishing credentials are required. GitHub repository/release
publication and marketplace submission are separate maintainer actions.

**Current validation:** automated launcher + native onboarding tests and local
packaged MCP acceptance; actual WorkBuddy install/login/reconnect UI acceptance
and public download remain pending. Do not advertise those as completed.

Reference: https://open.workbuddy.cn/docs/connector
