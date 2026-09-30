//! Persisted two-leg workflow. YAML chooses the signals; this runner carries receipts
//! from entry to exit and gives each leg one durable submission identity.
use crate::{
    agentic::{self, Config, Intent},
    agentic_handoff::Inbox,
    strategy::{self, RunReport, Snapshot},
};
use anyhow::{ensure, Context, Result};
use num_bigint::{BigInt, BigUint, Sign};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
};

mod replay;
pub use replay::run as replay;

pub const TEMPLATE: &str = include_str!("../flows/linked_stock_cycle.http.yml");
const CONTEXT: &str = "cycle_context";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    run_id: String,
    environment: String,
    snapshot_sha256: String,
    config_sha256: String,
    wallet_address: String,
    phase: String,
    baseline_receive: Option<String>,
    position_quantity: Option<String>,
    entry_cost: Option<String>,
    entry_average_price: Option<String>,
    entry: Intent,
    ready_to_submit: bool,
    buy: Option<Value>,
    sell: Option<Value>,
    last_evaluation: Option<RunReport>,
    last_error: Option<String>,
    updated_at: String,
    summary: Value,
}

struct Store {
    path: PathBuf,
    _guard: fs::File,
}
fn key(run_id: &str) -> Result<String> {
    ensure!(
        !run_id.is_empty()
            && run_id.len() <= 96
            && run_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "run_id must contain 1..96 ASCII letters, digits, dots, underscores or hyphens"
    );
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("flow-bnb-cycle-v1:{run_id}"))
    ))
}
fn state_path(c: &Config, run_id: &str) -> Result<PathBuf> {
    Ok(c.state_dir
        .join("strategy-cycles")
        .join(format!("{}.json", key(run_id)?)))
}
impl Store {
    fn open(c: &Config, run_id: &str) -> Result<Self> {
        let path = state_path(c, run_id)?;
        let dir = path.parent().unwrap();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        ensure!(
            !fs::symlink_metadata(dir)?.file_type().is_symlink(),
            "invalid cycle directory"
        );
        let lock = path.with_extension("writing");
        if lock.try_exists()? {
            ensure!(
                !fs::symlink_metadata(&lock)?.file_type().is_symlink(),
                "invalid cycle synchronization file"
            );
        }
        let guard = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(lock)?;
        guard
            .try_lock()
            .context("this cycle is already advancing in another process")?;
        Ok(Self {
            path,
            _guard: guard,
        })
    }
    fn read(&self) -> Result<Option<State>> {
        if !self.path.try_exists()? {
            return Ok(None);
        }
        read_state(&self.path).map(Some)
    }
    fn save(&self, state: &mut State) -> Result<()> {
        state.updated_at = chrono::Utc::now().to_rfc3339();
        let bytes = serde_json::to_vec_pretty(state)?;
        ensure!(bytes.len() <= 1_048_576, "cycle record too large");
        let temp = self.path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        let saved = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            fs::File::open(self.path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if saved.is_err() {
            let _ = fs::remove_file(temp);
        }
        saved
    }
}
fn read_state(path: &PathBuf) -> Result<State> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 1_048_576,
        "invalid cycle state file"
    );
    let state: State = serde_json::from_slice(&fs::read(path)?)
        .context("cycle state is unreadable; do not reset the run or replay its orders")?;
    ensure!(state.schema_version == 1, "unsupported cycle state version");
    Ok(state)
}
pub fn is_terminal(phase: &str) -> bool {
    matches!(
        phase,
        "completed" | "completed_with_discrepancy" | "needs_attention" | "stopped"
    )
}
pub fn status(c: &Config, run_id: &str) -> Result<Value> {
    let s = read_state(&state_path(c, run_id)?)?;
    ensure!(
        s.run_id == run_id && s.wallet_address.eq_ignore_ascii_case(&c.wallet_address),
        "cycle identity mismatch"
    );
    serde_json::to_value(s).map_err(Into::into)
}

fn entry_intent(base: &Snapshot) -> Result<Intent> {
    ensure!(
        !base.inputs.contains_key(CONTEXT),
        "cycle_context belongs to the runner, not caller inputs"
    );
    let (doc, _) = strategy::check(&base.yaml)?;
    ensure!(
        doc.flow.inputs.iter().any(|i| i.name == CONTEXT),
        "flow must declare cycle_context"
    );
    let value = base
        .inputs
        .get("entry_intent")
        .cloned()
        .or_else(|| {
            doc.flow
                .inputs
                .iter()
                .find(|i| i.name == "entry_intent")
                .and_then(|i| i.default.clone())
        })
        .context("cycle flow must declare entry_intent")?;
    serde_json::from_value(value).map_err(Into::into)
}
fn exit_intent(s: &State) -> Result<Intent> {
    Ok(Intent {
        from_token: s.entry.to_token.clone(),
        to_token: s.entry.from_token.clone(),
        amount: s
            .position_quantity
            .clone()
            .context("entry has no confirmed quantity")?,
        slippage_bps: s.entry.slippage_bps,
    })
}
fn contextual(base: &Snapshot, s: &State) -> Result<Snapshot> {
    let phase = match s.phase.as_str() {
        "buying" => "waiting_entry",
        "selling" => "holding",
        p => p,
    };
    let mut inputs = base.inputs.clone();
    inputs.insert(CONTEXT.into(), json!({
        "phase":phase,"baseline_receive":s.baseline_receive.as_deref().unwrap_or("0"),
        "entry_cost":s.entry_cost.as_deref().unwrap_or("0"),
        "exit_intent":if s.position_quantity.is_some() { serde_json::to_value(exit_intent(s)?)? } else { json!({}) }
    }));
    Snapshot::new(&base.yaml, inputs)
}

trait Backend: Send + Sync {
    fn environment(&self) -> &'static str;
    fn evaluate(&self, snapshot: &Snapshot) -> impl Future<Output = Result<RunReport>> + Send;
    fn order(
        &self,
        snapshot: &Snapshot,
        request_id: &str,
        execute: bool,
    ) -> impl Future<Output = Result<Value>> + Send;
}
struct Live {
    config: Config,
    inbox: Inbox,
}
impl Backend for Live {
    fn environment(&self) -> &'static str {
        "live"
    }
    async fn evaluate(&self, snapshot: &Snapshot) -> Result<RunReport> {
        strategy::run(&self.config, snapshot).await
    }
    async fn order(&self, snapshot: &Snapshot, request_id: &str, execute: bool) -> Result<Value> {
        if let Some(v) = self.inbox.status_for_request(request_id)? {
            ensure!(
                v["strategy"]["snapshot_sha256"] == snapshot.binding()?.snapshot_sha256,
                "existing order belongs to a different cycle snapshot"
            );
            if v["state"] == "queued" && execute {
                return self
                    .inbox
                    .execute_strategy(request_id.into(), snapshot.clone(), false)
                    .await;
            }
            if v["result"]["order_id"].is_string()
                && !matches!(
                    v["state"].as_str(),
                    Some("completed" | "settled_with_discrepancy")
                )
            {
                return self
                    .inbox
                    .refresh(crate::strategy_ops::field(&v, "intent_id")?)
                    .await;
            }
            return Ok(v);
        }
        if !execute {
            return Ok(json!({"state":"awaiting_execution"}));
        }
        // Check that the planned entry can be unwound under the configured token limit.
        if request_id.ends_with(".buy") {
            let d = strategy::run(&self.config, snapshot)
                .await?
                .decision
                .context("entry decision missing")?;
            if d.triggered {
                let quote = agentic::strategy_quote(&self.config, &d.intent).await?;
                self.config.rules(&Intent {
                    from_token: d.intent.to_token.clone(), to_token: d.intent.from_token.clone(),
                    amount: crate::strategy_ops::field(&quote, "toCoinAmount")?.into(), slippage_bps: d.intent.slippage_bps,
                }).context("planned position exceeds the configured exit limit; adjust the entry amount or local limit before trading")?;
            }
        }
        self.inbox
            .execute_strategy(request_id.into(), snapshot.clone(), false)
            .await
    }
}

pub async fn step(c: Config, base: Snapshot, run_id: &str, execute: bool) -> Result<Value> {
    let backend = Live {
        inbox: Inbox::open(c.clone())?,
        config: c.clone(),
    };
    advance(&c, &base, run_id, execute, &backend).await
}

async fn advance<B: Backend>(
    c: &Config,
    base: &Snapshot,
    run_id: &str,
    execute: bool,
    backend: &B,
) -> Result<Value> {
    let entry = entry_intent(base)?;
    c.rules(&entry)?;
    let store = Store::open(c, run_id)?;
    let fingerprint = base.binding()?.snapshot_sha256;
    let config_sha256 = agentic::digest(c)?;
    let mut s = match store.read()? {
        Some(s) => {
            ensure!(s.run_id == run_id && s.environment == backend.environment() && s.snapshot_sha256 == fingerprint
                && s.config_sha256 == config_sha256 && s.wallet_address.eq_ignore_ascii_case(&c.wallet_address),
                "cycle source, inputs, environment or wallet configuration changed; inspect the existing cycle instead of resetting it");
            s
        }
        None => State {
            schema_version: 1,
            run_id: run_id.into(),
            environment: backend.environment().into(),
            snapshot_sha256: fingerprint,
            config_sha256,
            wallet_address: c.wallet_address.clone(),
            phase: "initializing".into(),
            baseline_receive: None,
            position_quantity: None,
            entry_cost: None,
            entry_average_price: None,
            entry,
            ready_to_submit: false,
            buy: None,
            sell: None,
            last_evaluation: None,
            last_error: None,
            updated_at: String::new(),
            summary: Value::Null,
        },
    };
    if is_terminal(&s.phase) {
        return Ok(serde_json::to_value(s)?);
    }
    s.last_error = None;
    let result = advance_inner(c, base, &mut s, execute, backend, &store).await;
    if let Err(ref e) = result {
        s.last_error = Some(e.to_string());
    }
    store.save(&mut s)?;
    result?;
    Ok(serde_json::to_value(s)?)
}

async fn advance_inner<B: Backend>(
    c: &Config,
    base: &Snapshot,
    s: &mut State,
    execute: bool,
    backend: &B,
    store: &Store,
) -> Result<()> {
    if !matches!(s.phase.as_str(), "buying" | "selling") {
        let report = backend.evaluate(&contextual(base, s)?).await?;
        s.last_evaluation = Some(report.clone());
        ensure!(
            report.success,
            "strategy evaluation failed: {}",
            report.error.as_deref().unwrap_or("inspect steps")
        );
        if s.phase == "initializing" {
            ensure!(
                report.decision.is_none(),
                "initialization must not emit a trade decision"
            );
            let value = report
                .outputs
                .get("signal_receive")
                .and_then(Value::as_str)
                .context("initialization must output signal_receive")?;
            ensure!(
                agentic::units(value, 36)? > BigUint::from(0u8),
                "invalid baseline quote"
            );
            s.baseline_receive = Some(value.into());
            s.phase = "waiting_entry".into();
            return Ok(());
        }
        let d = report
            .decision
            .context("cycle must emit a triggered or untriggered decision")?;
        let expected = if s.phase == "holding" {
            exit_intent(s)?
        } else {
            s.entry.clone()
        };
        ensure!(
            serde_json::to_value(&d.intent)? == serde_json::to_value(expected)?,
            "cycle decision differs from the entry or confirmed position"
        );
        s.ready_to_submit = d.triggered;
        if !d.triggered || !execute {
            return Ok(());
        }
        s.phase = if s.phase == "holding" {
            "selling"
        } else {
            "buying"
        }
        .into();
        // Persist the phase BEFORE any external submission; a restart reuses this leg's ID.
        store.save(s)?;
    }
    let is_buy = s.phase == "buying";
    let id = format!(
        "cycle-{}.{}",
        key(&s.run_id)?,
        if is_buy { "buy" } else { "sell" }
    );
    let result = backend.order(&contextual(base, s)?, &id, execute).await?;
    if is_buy {
        s.buy = Some(result.clone());
    } else {
        s.sell = Some(result.clone());
    }
    match result["state"].as_str().unwrap_or("unknown") {
        "completed" | "settled_with_discrepancy" => {
            if let Err(e) = apply_settlement(c, s, &result, is_buy) {
                s.phase = "needs_attention".into();
                return Err(e);
            }
            s.ready_to_submit = false;
        }
        "not_triggered" if result.get("intent_id").is_none() => {
            s.phase = if is_buy { "waiting_entry" } else { "holding" }.into();
            s.ready_to_submit = false;
        }
        "queued"
        | "awaiting_execution"
        | "executing"
        | "pending"
        | "submitted"
        | "verifying_settlement" => (),
        "not_triggered" | "blocked" | "strategy_blocked" | "not_submitted" | "cancelled"
        | "order_failed" => {
            s.phase = "stopped".into();
            s.last_error = Some("This leg ended without a completed trade. Inspect its existing record; no replacement order was created.".into());
        }
        _ if result["result"]["order_id"].is_string() => (),
        _ => {
            s.phase = "needs_attention".into();
            s.last_error = Some("Order outcome is unknown. Inspect existing records; this cycle will not replay the order.".into());
        }
    }
    Ok(())
}

fn apply_settlement(c: &Config, s: &mut State, order: &Value, is_buy: bool) -> Result<()> {
    let expected = if is_buy {
        s.entry.clone()
    } else {
        exit_intent(s)?
    };
    ensure!(
        order["intent"] == serde_json::to_value(&expected)?,
        "settlement intent mismatch"
    );
    let settlement = &order["result"]["settlement"];
    let source = if s.environment == "live" {
        "receipt_transfer_logs"
    } else {
        "simulated_receipt_transfer_logs"
    };
    ensure!(settlement["source"] == source, "settlement source mismatch");
    let sold = crate::strategy_ops::field(settlement, "sold")?;
    let received = crate::strategy_ops::field(settlement, "received")?;
    let (from, to) = c.rules(&expected)?;
    let sold_units = agentic::units(sold, from.decimals)?;
    ensure!(
        sold_units > BigUint::from(0u8)
            && sold_units <= agentic::units(&expected.amount, from.decimals)?
            && agentic::units(received, to.decimals)? > BigUint::from(0u8),
        "invalid actual settlement amounts"
    );
    if is_buy {
        s.position_quantity = Some(received.into());
        s.entry_cost = Some(sold.into());
        let price =
            agentic::units(sold, 36)? * BigUint::from(10u8).pow(18) / agentic::units(received, 36)?;
        s.entry_average_price = Some(agentic::decimal(&price, 18));
        s.phase = "holding".into();
        // Amount discrepancies remain in the entry result. The separate exit uses only
        // receipt-confirmed inventory; it never retries or tops up the entry.
    } else {
        let residual = agentic::units(&expected.amount, from.decimals)? - sold_units;
        let cash_change = BigInt::from(agentic::units(received, 36)?)
            - BigInt::from(agentic::units(
                s.entry_cost.as_deref().context("missing entry cost")?,
                36,
            )?);
        let change = agentic::decimal(cash_change.magnitude(), 36);
        s.summary = json!({"entry_spent":s.entry_cost,"exit_received":received,
            "remaining_position":agentic::decimal(&residual, from.decimals),
            "cash_change_excluding_gas":if cash_change.sign()==Sign::Minus {format!("-{change}")} else {change},
            "gas_included":false});
        let discrepancy = residual > BigUint::from(0u8)
            || order["state"] == "settled_with_discrepancy"
            || s.buy
                .as_ref()
                .is_some_and(|v| v["state"] == "settled_with_discrepancy");
        s.phase = if discrepancy {
            "completed_with_discrepancy"
        } else {
            "completed"
        }
        .into();
    }
    Ok(())
}
