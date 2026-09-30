//! Local onboarding. Installation and authentication never submit a transaction.
use std::{
    env, fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, process::Command};

const BAW_VERSION: &str = "1.10.0";
const NODE_VERSION: &str = "22.23.3";

fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.permissions().mode() & 0o077 == 0,
        "{} must be a private directory (0700), not a symlink",
        path.display()
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    c.env_remove("BINANCE_WEB3_API_KEY")
        .env_remove("BINANCE_WEB3_SECRET_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    c
}

async fn output(mut c: Command, seconds: u64) -> Result<Vec<u8>> {
    let mut child = c
        .spawn()
        .context("无法启动依赖程序；请运行 flow-bnb doctor")?;
    let run = async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("missing stdout")?
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(bytes.len() <= 1_048_576, "dependency output exceeded 1 MiB");
        ensure!(
            child.wait().await?.success(),
            "依赖命令失败；请检查网络后重新运行 setup（已有配置会保留）"
        );
        Ok(bytes)
    };
    tokio::time::timeout(Duration::from_secs(seconds), run)
        .await
        .context("操作超时；请重新运行 setup。登录将使用新的配对码")?
}

fn executable(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|p| env::split_paths(&p).collect::<Vec<_>>())
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .map(|p| p.join(name))
        .find(|p| p.is_absolute() && p.is_file())
}

fn node_archive(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    Ok(match (os, arch) {
        ("macos", "aarch64") => (
            "darwin-arm64",
            "23b25245dcfb9af7262f8ff142e9e2e0af025368117329e7a7458a51e5922f53",
        ),
        ("macos", "x86_64") => (
            "darwin-x64",
            "8a677b0219178efd6eb0e475457c4afb452b521a92f6e67845a73bd85727f2a8",
        ),
        ("linux", "aarch64") => (
            "linux-arm64",
            "5ced2d48d1d7198739b7f86804de0171aefb6823b684b12341d3321afc3cb0b2",
        ),
        ("linux", "x86_64") => (
            "linux-x64",
            "1084aa36196bba4c3a5e69a1ee388a6e4ff729dad09445fbcd434b28fe3c24af",
        ),
        _ => bail!("自动安装支持 macOS/Linux arm64/x64；此平台请配置自己的 baw"),
    })
}

fn verify_archive(bytes: &[u8], expected: &str) -> Result<()> {
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == expected,
        "Node 下载校验失败；未安装，请重新运行 setup"
    );
    Ok(())
}

async fn usable_node(node: &Path) -> bool {
    let mut c = command(node);
    c.arg("--version");
    output(c, 10)
        .await
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|v| {
            v.trim()
                .trim_start_matches('v')
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
        })
        .is_some_and(|v| v >= 18)
}

// Install in a staging directory. A failed install cannot replace a working version.
async fn runtime(home: &Path) -> Result<PathBuf> {
    if let Some(node) = executable("node") {
        if usable_node(&node).await && node.parent().is_some_and(|p| p.join("npm").is_file()) {
            return Ok(node);
        }
    }
    let (platform, checksum) = node_archive(env::consts::OS, env::consts::ARCH)?;
    let name = format!("node-v{NODE_VERSION}-{platform}");
    let dest = home.join(&name);
    let node = dest.join("bin/node");
    if usable_node(&node).await {
        return Ok(node);
    }
    ensure!(
        !dest.exists(),
        "本地 Node 安装损坏：{}；请检查后移走该目录再运行 setup",
        dest.display()
    );
    eprintln!("  正在准备本地 Node {NODE_VERSION}（无需全局安装或 sudo）…");
    let stage = stage(home)?;
    let archive = stage.join("node.tar.gz");
    let mut curl = command(&executable("curl").context("自动安装需要系统 curl")?);
    curl.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--retry",
        "2",
        "--max-time",
        "180",
        "--output",
    ])
    .arg(&archive)
    .arg(format!(
        "https://nodejs.org/dist/v{NODE_VERSION}/{name}.tar.gz"
    ));
    let install = async {
        output(curl, 240)
            .await
            .context("Node 下载失败；检查 nodejs.org 网络连接后重试 setup")?;
        verify_archive(&fs::read(&archive)?, checksum)?;
        let mut tar = command(&executable("tar").context("自动安装需要系统 tar")?);
        tar.arg("-xzf").arg(&archive).arg("-C").arg(&stage);
        output(tar, 60).await?;
        ensure!(
            usable_node(&stage.join(&name).join("bin/node")).await,
            "下载的 Node 无法运行（Linux 需要兼容 glibc）"
        );
        fs::rename(stage.join(&name), &dest)?;
        Ok(node)
    }
    .await;
    let _ = fs::remove_dir_all(&stage);
    install
}

fn stage(home: &Path) -> Result<PathBuf> {
    let p = home.join(format!(
        ".install-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::DirBuilder::new().mode(0o700).create(&p)?;
    Ok(p)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn prepend_node(c: &mut Command, node: &Path) -> Result<()> {
    c.env("PATH", node_path(node)?);
    Ok(())
}
fn node_path(node: &Path) -> Result<std::ffi::OsString> {
    let mut paths = vec![node
        .parent()
        .context("missing Node directory")?
        .to_path_buf()];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    Ok(env::join_paths(paths)?)
}

async fn install_baw(home: &Path, node: &Path) -> Result<PathBuf> {
    let dest = home.join(format!("baw-{BAW_VERSION}"));
    let launcher = dest.join("baw");
    if launcher.is_file() {
        check_version(&launcher, node).await?;
        return Ok(launcher);
    }
    ensure!(
        !dest.exists(),
        "本地 baw 安装损坏：{}；请检查后移走该目录再运行 setup",
        dest.display()
    );
    eprintln!("  正在安装 Binance Agentic Wallet {BAW_VERSION}…");
    let stage = stage(home)?;
    let install = async {
        let npm = node.parent().context("missing Node directory")?.join("npm");
        let mut c = command(node);
        c.arg(fs::canonicalize(&npm).context("Node 对应的 npm 不可用")?)
            .args([
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--save-exact",
                "--registry",
                "https://registry.npmjs.org",
                "--prefix",
            ])
            .arg(&stage)
            .arg(format!("@binance/agentic-wallet@{BAW_VERSION}"));
        prepend_node(&mut c, node)?;
        output(c, 300)
            .await
            .context("baw 安装失败；检查 registry.npmjs.org 网络连接后重试 setup")?;
        let entry = dest.join("node_modules/@binance/agentic-wallet/dist/index.js");
        let launcher_text = format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            shell_quote(&node.to_string_lossy()),
            shell_quote(&entry.to_string_lossy())
        );
        write_new(&stage.join("baw"), launcher_text.as_bytes(), 0o700)?;
        // Check the staged package before publishing it at the stable path.
        check_version(&stage.join("node_modules/.bin/baw"), node).await?;
        fs::rename(&stage, &dest)?;
        Ok(launcher)
    }
    .await;
    if stage.exists() {
        let _ = fs::remove_dir_all(stage);
    }
    install
}

async fn check_version(baw: &Path, node: &Path) -> Result<()> {
    let mut c = command(baw);
    c.arg("--version");
    prepend_node(&mut c, node)?;
    let bytes = output(c, 20).await?;
    let v = String::from_utf8(bytes)?;
    ensure!(
        v.trim().trim_start_matches('v') == BAW_VERSION,
        "baw 版本不匹配：预期 {BAW_VERSION}，实际 {}。保留现有配置，请核对兼容性",
        v.trim()
    );
    Ok(())
}

async fn wallet(baw: &Path, node: &Path, args: &[&str], timeout: u64) -> Result<Value> {
    let mut c = command(baw);
    c.args(args).arg("--json");
    prepend_node(&mut c, node)?;
    let v: Value =
        serde_json::from_slice(&output(c, timeout).await?).context("baw 返回了无效 JSON")?;
    ensure!(
        v["success"] == true,
        "钱包操作未完成；请运行 setup 重新登录（不会提交交易）"
    );
    Ok(v["data"].clone())
}

fn login_details(data: &Value) -> Result<(&str, &str, &str)> {
    let link = data["urlForWeb"].as_str().context("missing login URL")?;
    let url = url::Url::parse(link)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("web3.binance.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none(),
        "拒绝打开非币安官方登录链接"
    );
    let code = data["pairingCode"]
        .as_str()
        .context("missing pairing code")?;
    let id = data["qrCodeId"].as_str().context("missing QR code ID")?;
    ensure!(
        !code.is_empty()
            && !id.is_empty()
            && !code.chars().any(char::is_control)
            && !link.chars().any(char::is_control),
        "invalid login details"
    );
    Ok((link, code, id))
}

pub type WalletProgress = Arc<Mutex<Value>>;

fn progress(state: Option<&WalletProgress>, value: Value) {
    if let Some(state) = state {
        *state.lock().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

async fn login(
    baw: &Path,
    node: &Path,
    no_open: bool,
    state: Option<&WalletProgress>,
) -> Result<()> {
    let data = wallet(baw, node, &["auth", "signin"], 30).await?;
    if data["status"] != "ALREADY_CONNECTED" {
        let (link, code, id) = login_details(&data)?;
        progress(
            state,
            json!({"phase":"awaiting_wallet", "login_url":link, "pairing_code":code,
            "message":"请打开币安官方链接，在手机核对配对码；完成后调用 get_bnb_connection。配对最长等待 5 分钟。"}),
        );
        eprintln!("\n请在币安 App 中核对配对码：{code}\n登录链接： {link} \n等待手机确认，最长 5 分钟；请保持连接窗口开启…");
        if !no_open {
            if let Some(opener) = executable(if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }) {
                let mut c = command(&opener);
                c.arg(link);
                if output(c, 10).await.is_err() {
                    eprintln!("浏览器未能自动打开，请使用上方完整链接。");
                }
            }
        }
        wallet(baw, node, &["auth", "verify", "--qrCodeId", id], 330)
            .await
            .context("登录未完成或配对码过期；重新运行 setup 获取新码")?;
    }
    ensure!(
        wallet(baw, node, &["wallet", "status"], 30).await?["status"] == "CONNECTED",
        "手机确认后本机仍未连接；重新运行 setup 获取新的登录码"
    );
    Ok(())
}

fn bsc_address(data: &Value) -> Result<String> {
    let addresses = data["addresses"]
        .as_array()
        .context("missing wallet addresses")?;
    let found: Vec<_> = addresses
        .iter()
        .filter(|v| v["binanceChainId"] == "56" || v["binanceChainId"] == 56)
        .collect();
    ensure!(found.len() == 1, "钱包未返回唯一的 BSC 地址");
    let a = found[0]["address"]
        .as_str()
        .context("missing BSC address")?;
    ensure!(
        a.len() == 42 && a.starts_with("0x") && a[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid BSC address"
    );
    Ok(a.to_owned())
}

pub fn config_path(root: &Path) -> PathBuf {
    env::var_os("FLOW_BNB_AGENTIC_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".flow-bnb/agentic.json"))
}

/// Prepare Node only. The desktop bootstrap uses this before any wallet access.
pub async fn prepare_runtime(config: &Path) -> Result<PathBuf> {
    let parent = config.parent().context("missing configuration directory")?;
    private_dir(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(parent.join("setup.lock"))?;
    lock.try_lock()
        .context("另一个 setup 正在运行；请等待它完成")?;
    let home = parent.join("managed");
    private_dir(&home)?;
    runtime(&home).await
}

pub async fn setup(config: &Path, no_login: bool, no_open: bool) -> Result<()> {
    setup_for_root(&env::current_dir()?, config, no_login, no_open, None).await
}

pub async fn setup_for_root(
    root: &Path,
    config: &Path,
    no_login: bool,
    no_open: bool,
    state: Option<&WalletProgress>,
) -> Result<()> {
    let root = fs::canonicalize(root)?;
    let config = if config.is_absolute() {
        config.to_owned()
    } else {
        root.join(config)
    };
    let parent = config.parent().context("missing configuration directory")?;
    ensure!(
        config != parent.join("mcp.json") && config != parent.join("setup.lock"),
        "配置文件名不能使用 setup 保留的 mcp.json 或 setup.lock"
    );
    private_dir(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(parent.join("setup.lock"))?;
    lock.try_lock()
        .context("另一个 setup 正在运行；请等待它完成")?;
    let home = parent.join("managed");
    private_dir(&home)?;
    eprintln!("[1/3] 检查本地运行环境…");
    let node = runtime(&home).await?;
    // Read without rewriting: historical evidence and queued orders bind the config digest.
    let existing = if config.try_exists()? {
        Some(crate::agentic::Config::read(&config)?)
    } else {
        None
    };
    let baw = if let Some(c) = &existing {
        check_version(&c.executable, &node).await?;
        eprintln!("  复用已有 baw {BAW_VERSION}；交易配置保持原样。");
        c.executable.clone()
    } else {
        install_baw(&home, &node).await?
    };
    eprintln!("[2/3] 检查 Agentic Wallet 登录…");
    if wallet(&baw, &node, &["wallet", "status"], 30).await?["status"] != "CONNECTED" {
        if no_login {
            eprintln!("依赖已准备好。尚未登录；运行 flow-bnb setup 完成手机配对。");
            return Ok(());
        }
        login(&baw, &node, no_open, state).await?;
    }
    let address = bsc_address(&wallet(&baw, &node, &["wallet", "address"], 30).await?)?;
    if let Some(c) = &existing {
        ensure!(
            c.wallet_address.eq_ignore_ascii_case(&address),
            "当前登录钱包 {address} 与配置 {} 不符；未修改配置，请切回原钱包后运行 setup",
            c.wallet_address
        );
    } else {
        let mut c: Value = serde_json::from_str(include_str!("../examples/agentic-config.json"))?;
        c["executable"] = json!(baw);
        c["wallet_address"] = json!(address);
        c["state_dir"] = json!(parent.join("agentic-state"));
        c["tokens"][1]["max_sell_amount"] = json!("0.01");
        private_dir(&parent.join("agentic-state"))?;
        let draft = stage(&home)?;
        let publish = (|| -> Result<()> {
            write_new(
                &draft.join("config.json"),
                &serde_json::to_vec_pretty(&c)?,
                0o600,
            )?;
            // Publish a complete file atomically, and never replace an existing policy.
            fs::hard_link(draft.join("config.json"), &config)?;
            Ok(())
        })();
        let _ = fs::remove_dir_all(draft);
        publish?;
        eprintln!("  初始限额：单笔最多 6 USDT / 0.01 AAPLon，滑点最多 0.5%；执行工具会直接下单。");
    }
    crate::agentic::Config::read(&config)?;
    eprintln!("[3/3] 生成 MCP 客户端配置…");
    let mcp = json!({"mcpServers":{"flow-bnb":{
        "command":env::current_exe()?, "args":["mcp","--root",root],
        "env":{"FLOW_BNB_AGENTIC_CONFIG":config,"PATH":node_path(&node)?.to_string_lossy()}
    }}});
    let mcp_path = parent.join("mcp.json");
    // This is generated output, not the user's client settings. Replace atomically.
    let stage = stage(&home)?;
    write_new(
        &stage.join("mcp.json"),
        &serde_json::to_vec_pretty(&mcp)?,
        0o600,
    )?;
    fs::rename(stage.join("mcp.json"), &mcp_path)?;
    fs::remove_dir(stage)?;
    eprintln!("\n已就绪 · BSC 钱包 {address}\n钱包配置：{}\nMCP 配置：{}\n返回对话即可询价或请求交易。执行工具直接下单，无需另开操作员终端。手动配置客户端时使用上述 MCP 配置。", config.display(), mcp_path.display());
    Ok(())
}

fn installed_node(config: &Path) -> Result<PathBuf> {
    executable("node")
        .or_else(|| {
            let (platform, _) = node_archive(env::consts::OS, env::consts::ARCH).ok()?;
            let p = config
                .parent()?
                .join("managed")
                .join(format!("node-v{NODE_VERSION}-{platform}/bin/node"));
            p.is_file().then_some(p)
        })
        .context("没有找到 Node；请重新连接以准备依赖")
}

/// No installation, login, configuration writes or trading. Fits the connector's
/// ten-second status budget even if the wallet API stalls.
pub async fn connection_status(config: &Path) -> Result<String> {
    let c = crate::agentic::Config::read(config)?;
    let node = installed_node(config)?;
    ensure!(
        wallet(&c.executable, &node, &["wallet", "status"], 3).await?["status"] == "CONNECTED",
        "钱包未连接"
    );
    let address = bsc_address(&wallet(&c.executable, &node, &["wallet", "address"], 3).await?)?;
    ensure!(
        address.eq_ignore_ascii_case(&c.wallet_address),
        "钱包与配置不符"
    );
    Ok(address)
}

/// Sign out only. Never delete a policy or a lock to make reconnection succeed.
pub async fn disconnect(config: &Path) -> Result<()> {
    let parent = config.parent().context("missing configuration directory")?;
    if !parent.exists() {
        return Ok(());
    }
    private_dir(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(parent.join("setup.lock"))?;
    lock.try_lock()
        .context("钱包正在配对，请先取消或完成配对后再断开")?;
    let executable = if config.exists() {
        crate::agentic::Config::read(config)?.executable
    } else {
        parent.join(format!("managed/baw-{BAW_VERSION}/baw"))
    };
    if !executable.exists() {
        return Ok(());
    }
    let node = installed_node(config)?;
    wallet(&executable, &node, &["auth", "signout"], 25).await?;
    eprintln!("钱包已登出；交易规则、授权记录和未决订单仍保留。已提交的订单不会撤回。");
    Ok(())
}

pub async fn doctor(config: &Path) -> Result<()> {
    eprintln!("检查配置：{}", config.display());
    let c = crate::agentic::Config::read(config)
        .context("尚未完成配置或配置无效；请运行 flow-bnb setup")?;
    let node = executable("node")
        .or_else(|| {
            let (platform, _) = node_archive(env::consts::OS, env::consts::ARCH).ok()?;
            let p = config
                .parent()?
                .join("managed")
                .join(format!("node-v{NODE_VERSION}-{platform}/bin/node"));
            p.is_file().then_some(p)
        })
        .context("没有找到 Node；请运行 flow-bnb setup 自动准备")?;
    check_version(&c.executable, &node).await?;
    ensure!(
        wallet(&c.executable, &node, &["wallet", "status"], 30).await?["status"] == "CONNECTED",
        "钱包未连接；请运行 flow-bnb setup 登录"
    );
    let a = bsc_address(&wallet(&c.executable, &node, &["wallet", "address"], 30).await?)?;
    ensure!(
        a.eq_ignore_ascii_case(&c.wallet_address),
        "当前钱包与配置不符；请切回原钱包"
    );
    eprintln!(
        "✓ baw {BAW_VERSION} · 已连接 · BSC {a}\n✓ 配置有效；单笔滑点上限 {} bps",
        c.max_slippage_bps
    );
    eprintln!("此检查未下单；余额、报价和代币审计在交易准备时检查。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_rejects_spoofed_hosts_and_preserves_pairing_code() {
        let mut d = json!({"urlForWeb":"https://web3.binance.com/en/agent-login?a=1%2F2", "pairingCode":"001234", "qrCodeId":"id"});
        assert_eq!(login_details(&d).unwrap().1, "001234");
        for url in [
            "https://web3.binance.com.evil.test/",
            "http://web3.binance.com/",
            "https://user@web3.binance.com/",
        ] {
            d["urlForWeb"] = json!(url);
            assert!(login_details(&d).is_err());
        }
    }
    #[test]
    fn rejects_corrupt_runtime_and_ambiguous_wallet() {
        assert!(verify_archive(b"broken", &"0".repeat(64)).is_err());
        assert!(node_archive("windows", "x86_64").is_err());
        let a =
            json!({"binanceChainId":"56","address":"0xDaD97288C1fcc449D499b7Aa578d1960fdAEeA23"});
        assert!(bsc_address(&json!({"addresses":[a.clone(),a.clone()]})).is_err());
        assert!(bsc_address(&json!({"addresses":[a]})).is_ok());
    }
    #[test]
    fn private_files_never_overwrite_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        write_new(&p, b"original", 0o600).unwrap();
        assert!(write_new(&p, b"replacement", 0o600).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"original");
        assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
    }
    #[tokio::test]
    async fn launcher_quotes_paths_and_works_without_node_on_path() {
        let dir = tempfile::Builder::new()
            .prefix("flow ' space ")
            .tempdir()
            .unwrap();
        let node = dir.path().join("node");
        write_new(&node, b"#!/bin/sh\nprintf '%s' \"$1\"\n", 0o700).unwrap();
        let entry = dir.path().join("a ' b.js");
        let launcher = dir.path().join("baw");
        write_new(
            &launcher,
            format!(
                "#!/bin/sh\nexec {} {} \"$@\"\n",
                shell_quote(node.to_str().unwrap()),
                shell_quote(entry.to_str().unwrap())
            )
            .as_bytes(),
            0o700,
        )
        .unwrap();
        let mut c = command(&launcher);
        c.env("PATH", "/nonexistent");
        assert_eq!(
            output(c, 5).await.unwrap(),
            entry.to_str().unwrap().as_bytes()
        );
    }
}
