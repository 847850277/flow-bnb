//! Operator-created, bounded strategy mandates. MCP can use or revoke a mandate,
//! but cannot create one, change its strategy, or increase its budget.
use crate::{
    agentic::{self, Config, Intent},
    strategy::{self, Binding, Snapshot},
};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_orders: u32,
    pub max_total_sell_amount: String,
    pub valid_for_minutes: u32,
    pub cooldown_seconds: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mandate {
    schema_version: u32,
    authorization_id: String,
    wallet_address: String,
    config_sha256: String,
    strategy: Binding,
    intent: Intent,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    limits: Limits,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    request_id: String,
    policy_sha256: String,
    state: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    reserved_at: Option<DateTime<Utc>>,
    reserved_amount: Option<String>,
    message: Option<String>,
}
struct Store {
    config: Config,
    dir: PathBuf,
    mandate: Mandate,
    hash: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn key(id: &str) -> Result<String> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "ID must be 1..128 ASCII letters, digits, dots, underscores or hyphens"
    );
    Ok(hash(id.as_bytes()))
}
fn private_dir(p: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)?;
    let m = fs::symlink_metadata(p)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.permissions().mode() & 0o077 == 0,
        "autonomy directory must be private and not a symlink"
    );
    Ok(())
}
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    let m = fs::symlink_metadata(p)?;
    ensure!(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.len() <= 1_048_576
            && m.permissions().mode() & 0o077 == 0,
        "invalid private autonomy file"
    );
    serde_json::from_slice(&fs::read(p)?)
        .context("autonomy record corrupt; inspect existing orders, do not retry")
}
fn write(p: &Path, value: &impl Serialize, replace: bool) -> Result<()> {
    let parent = p.parent().context("missing parent")?;
    let tmp = parent.join(format!(
        ".write-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| -> Result<()> {
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
        if replace {
            fs::rename(&tmp, p)?;
        } else {
            fs::hard_link(&tmp, p)?;
        }
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(tmp);
    result
}
fn same_intent(a: &Intent, b: &Intent) -> Result<bool> {
    Ok(serde_json::to_value(a)? == serde_json::to_value(b)?)
}

/// This is a local operator CLI operation, deliberately not an MCP tool.
/// Even a non-triggered decision can define the exact pair/amount being authorized.
pub async fn authorize(c: Config, id: String, snapshot: Snapshot, limits: Limits) -> Result<Value> {
    let report = strategy::run(&c, &snapshot).await?;
    ensure!(
        report.success,
        "strategy preview failed; authorization not created"
    );
    let decision=report.decision.context("strategy must always emit a decision (triggered=false when inactive) before it can be authorized")?;
    create(c, id, snapshot, decision.intent, limits)
}
fn create(
    c: Config,
    id: String,
    snapshot: Snapshot,
    intent: Intent,
    limits: Limits,
) -> Result<Value> {
    let name = key(&id)?;
    let (sell, _) = c.rules(&intent)?;
    ensure!(
        limits.max_orders > 0 && limits.max_orders <= 10_000,
        "max_orders must be 1..10000"
    );
    ensure!(
        limits.valid_for_minutes > 0 && limits.valid_for_minutes <= 43_200,
        "authorization validity must be 1 minute..30 days"
    );
    ensure!(
        agentic::units(&limits.max_total_sell_amount, sell.decimals)?
            >= agentic::units(&intent.amount, sell.decimals)?,
        "total sell budget must cover at least one order"
    );
    let base = c.state_dir.join("autonomy");
    private_dir(&base)?;
    let dir = base.join(name);
    private_dir(&dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("lock"))?;
    lock.try_lock().context("authorization is busy")?;
    ensure!(
        !dir.join("policy.json").try_exists()?,
        "authorization ID already exists; never overwrite a mandate or reset its budget"
    );
    let binding = snapshot.persist(&c)?;
    let now = Utc::now();
    let mandate = Mandate {
        schema_version: 1,
        authorization_id: id.clone(),
        wallet_address: c.wallet_address.clone(),
        config_sha256: agentic::digest(&c)?,
        strategy: binding,
        intent,
        created_at: now,
        expires_at: now + chrono::Duration::minutes(i64::from(limits.valid_for_minutes)),
        limits,
    };
    write(&dir.join("policy.json"), &mandate, false)?;
    drop(lock);
    status(&c, &id)
}
impl Store {
    fn open(c: &Config, id: &str) -> Result<Self> {
        let base = c.state_dir.join("autonomy");
        ensure!(
            base.is_dir() && !fs::symlink_metadata(&base)?.file_type().is_symlink(),
            "no authorizations; operator must run strategy-authorize first"
        );
        let dir = base.join(key(id)?);
        ensure!(
            dir.is_dir() && !fs::symlink_metadata(&dir)?.file_type().is_symlink(),
            "authorization not found"
        );
        let mandate: Mandate = read(&dir.join("policy.json"))?;
        ensure!(
            mandate.schema_version == 1 && mandate.authorization_id == id,
            "authorization identity mismatch"
        );
        let hash = hash(&serde_json::to_vec(&mandate)?);
        Ok(Self {
            config: c.clone(),
            dir,
            mandate,
            hash,
        })
    }
    fn lock(&self) -> Result<File> {
        let p = self.dir.join("lock");
        ensure!(
            !fs::symlink_metadata(&p)?.file_type().is_symlink(),
            "invalid authorization lock"
        );
        let f = fs::OpenOptions::new().read(true).write(true).open(p)?;
        f.try_lock().context("authorization is busy; query the existing execution instead of launching a replacement")?;
        Ok(f)
    }
    fn check_active(&self) -> Result<()> {
        let policy: Mandate = read(&self.dir.join("policy.json"))?;
        ensure!(
            hash(&serde_json::to_vec(&policy)?) == self.hash,
            "authorization changed; execution blocked"
        );
        ensure!(
            self.mandate.config_sha256 == agentic::digest(&self.config)?
                && self
                    .mandate
                    .wallet_address
                    .eq_ignore_ascii_case(&self.config.wallet_address),
            "wallet or operator configuration changed; authorization invalid"
        );
        ensure!(
            !self.dir.join("revoked.json").try_exists()?,
            "authorization revoked"
        );
        ensure!(
            !self.dir.join("halted.json").try_exists()?,
            "authorization halted; inspect existing execution reports"
        );
        ensure!(
            Utc::now() < self.mandate.expires_at,
            "authorization expired"
        );
        self.config.rules(&self.mandate.intent)?;
        self.mandate.strategy.load(&self.config)?;
        Ok(())
    }
    fn path(&self, request_id: &str, suffix: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.{}", key(request_id)?, suffix)))
    }
    fn attempts(&self) -> Result<Vec<Attempt>> {
        let mut out = vec![];
        for entry in fs::read_dir(&self.dir)? {
            let p = entry?.path();
            if p.file_name()
                .is_some_and(|s| s.to_string_lossy().ends_with(".attempt.json"))
            {
                let a: Attempt = read(&p)?;
                ensure!(
                    a.policy_sha256 == self.hash && self.path(&a.request_id, "attempt.json")? == p,
                    "execution record does not belong to this authorization"
                );
                ensure!(
                    a.reserved_amount.is_some() == a.reserved_at.is_some(),
                    "incomplete budget reservation"
                );
                out.push(a);
            }
        }
        ensure!(
            out.len() <= 1024,
            "authorization has too many evaluation records"
        );
        Ok(out)
    }
    fn usage(&self) -> Result<(u32, BigUint, Option<DateTime<Utc>>)> {
        let (sell, _) = self.config.rules(&self.mandate.intent)?;
        let mut total = BigUint::from(0u8);
        let mut count = 0;
        let mut last = None;
        for a in self.attempts()? {
            if let Some(amount) = a.reserved_amount {
                ensure!(
                    amount == self.mandate.intent.amount,
                    "unexpected reserved amount; inspect ledger"
                );
                total += agentic::units(&amount, sell.decimals)?;
                count += 1;
                last = last.max(a.reserved_at);
            }
        }
        Ok((count, total, last))
    }
    fn budget(&self) -> Result<()> {
        self.check_active()?;
        let (count, total, last) = self.usage()?;
        let (sell, _) = self.config.rules(&self.mandate.intent)?;
        ensure!(
            count < self.mandate.limits.max_orders,
            "authorized order count exhausted"
        );
        ensure!(
            total + agentic::units(&self.mandate.intent.amount, sell.decimals)?
                <= agentic::units(&self.mandate.limits.max_total_sell_amount, sell.decimals)?,
            "authorized cumulative sell budget exhausted"
        );
        if let Some(last) = last {
            ensure!(
                Utc::now().signed_duration_since(last).num_seconds()
                    >= i64::from(self.mandate.limits.cooldown_seconds),
                "authorization cooldown has not elapsed"
            );
        }
        ensure!(
            !self
                .config
                .state_dir
                .join(format!(
                    "agentic-{}.lock",
                    self.config.wallet_address.to_lowercase()
                ))
                .try_exists()?,
            "wallet has unresolved submission; inspect previous order first"
        );
        Ok(())
    }
    fn halt(&self, message: &str) -> Result<()> {
        if !self.dir.join("halted.json").try_exists()? {
            write(
                &self.dir.join("halted.json"),
                &json!({"at":Utc::now(),"message":message}),
                false,
            )?;
        }
        Ok(())
    }
}

/// An in-memory capability with a held exclusive lock. It cannot be deserialized
/// from tool arguments; only a validated, operator-owned mandate creates it.
pub(crate) struct Permit {
    store: Store,
    attempt: Attempt,
    _lock: File,
}
impl Permit {
    pub(crate) fn reserve(&mut self, c: &Config, i: &Intent, binding: &Binding) -> Result<()> {
        ensure!(
            agentic::digest(c)? == self.store.mandate.config_sha256
                && same_intent(i, &self.store.mandate.intent)?
                && binding.snapshot_sha256 == self.store.mandate.strategy.snapshot_sha256,
            "order is outside the authorized strategy"
        );
        ensure!(
            self.attempt.reserved_amount.is_none(),
            "execution already reserved; never submit twice"
        );
        self.store.budget()?;
        self.attempt.reserved_amount = Some(i.amount.clone());
        self.attempt.reserved_at = Some(Utc::now());
        self.attempt.state = "submitting".into();
        self.save()
    }
    fn save(&mut self) -> Result<()> {
        self.attempt.updated_at = Utc::now();
        write(
            &self.store.path(&self.attempt.request_id, "attempt.json")?,
            &self.attempt,
            true,
        )
    }
    fn finish(&mut self, state: &str, message: Option<String>) -> Result<()> {
        self.attempt.state = state.into();
        self.attempt.message = message;
        self.save()?;
        if !matches!(state, "completed" | "not_triggered") {
            self.store.halt(
                "Execution stopped or needs review; reservations are not refunded automatically",
            )?;
        }
        Ok(())
    }
    async fn execute(&mut self) -> Result<()> {
        self.store.check_active()?;
        let snapshot = self.store.mandate.strategy.load(&self.store.config)?;
        let evaluation = strategy::run(&self.store.config, &snapshot).await?;
        ensure!(evaluation.success, "strategy evaluation failed");
        let decision = evaluation
            .decision
            .context("authorized strategy produced no decision")?;
        ensure!(
            same_intent(&decision.intent, &self.store.mandate.intent)?,
            "strategy selected an unauthorized pair or amount"
        );
        if !decision.triggered {
            return self.finish("not_triggered", None);
        }
        let path = self.store.path(&self.attempt.request_id, "report.json")?;
        let c = self.store.config.clone();
        let i = self.store.mandate.intent.clone();
        let binding = self.store.mandate.strategy.clone();
        let report = agentic::run_authorized(c, i, &path, binding, self).await?;
        self.finish(&report.state, report.error)
    }
}

fn claim(c: &Config, id: &str, request_id: &str) -> Result<Option<Permit>> {
    let store = Store::open(c, id)?;
    let path = store.path(request_id, "attempt.json")?;
    if path.try_exists()? {
        return Ok(None);
    }
    let lock = store.lock()?;
    if path.try_exists()? {
        return Ok(None);
    }
    store.budget()?;
    let attempts = store.attempts()?;
    ensure!(attempts.len() < 1024, "authorization evaluation log full");
    // A terminated task is not a retry opportunity, even if it died before reserving.
    ensure!(
        !attempts
            .iter()
            .any(|a| !matches!(a.state.as_str(), "completed" | "not_triggered")),
        "previous execution was interrupted; inspect it before further automatic execution"
    );
    let now = Utc::now();
    let attempt = Attempt {
        request_id: request_id.into(),
        policy_sha256: store.hash.clone(),
        state: "evaluating".into(),
        created_at: now,
        updated_at: now,
        reserved_at: None,
        reserved_amount: None,
        message: None,
    };
    write(&path, &attempt, false)?;
    Ok(Some(Permit {
        store,
        attempt,
        _lock: lock,
    }))
}

/// Returns immediately for MCP; durable records survive cancellation or server restart.
pub fn start(c: Config, id: String, request_id: String) -> Result<Value> {
    if let Some(mut permit) = claim(&c, &id, &request_id)? {
        tokio::spawn(async move {
            if let Err(e) = permit.execute().await {
                let _ = permit.finish("needs_attention", Some(e.to_string()));
            }
        });
    }
    execution(&c, &id, &request_id)
}
/// Foreground CLI entry; no TTY and no CONFIRM are needed for an existing mandate.
pub async fn execute(c: Config, id: String, request_id: String) -> Result<Value> {
    if let Some(mut permit) = claim(&c, &id, &request_id)? {
        if let Err(e) = permit.execute().await {
            permit.finish("needs_attention", Some(e.to_string()))?;
        }
    }
    execution(&c, &id, &request_id)
}
pub fn status(c: &Config, id: &str) -> Result<Value> {
    let store = Store::open(c, id)?;
    let (orders, total, last) = store.usage()?;
    let (sell, _) = c.rules(&store.mandate.intent)?;
    let busy = store.lock().is_err();
    let interrupted = !busy
        && store
            .attempts()?
            .iter()
            .any(|a| !matches!(a.state.as_str(), "completed" | "not_triggered"));
    let blocked = store.budget().err().map(|e| e.to_string()).or_else(|| {
        if interrupted {
            Some("previous execution needs attention".into())
        } else if busy {
            Some("execution is currently active".into())
        } else {
            None
        }
    });
    Ok(
        json!({"authorization":store.mandate,"reserved_orders":orders,"reserved_sell_amount":agentic::decimal(&total,sell.decimals),
        "last_reserved_at":last,"eligible":blocked.is_none(),"blocker":blocked,"confirmation_required":false}),
    )
}
pub fn execution(c: &Config, id: &str, request_id: &str) -> Result<Value> {
    let store = Store::open(c, id)?;
    let a: Attempt = read(&store.path(request_id, "attempt.json")?)?;
    ensure!(
        a.request_id == request_id && a.policy_sha256 == store.hash,
        "execution binding mismatch"
    );
    let mut value = json!({"authorization_id":id,"execution_id":key(request_id)?,"request_id":request_id,"state":a.state,
        "reserved_amount":a.reserved_amount,"message":a.message,"retry_execution":false,"result":null});
    let path = store.path(request_id, "report.json")?;
    if path.try_exists()? {
        let file = File::open(&path)?;
        if file.try_lock_shared().is_ok() {
            let r: agentic::Report = read(&path)?;
            ensure!(
                r.config_sha256 == store.mandate.config_sha256
                    && r.wallet_address
                        .eq_ignore_ascii_case(&store.mandate.wallet_address)
                    && same_intent(&r.intent, &store.mandate.intent)?,
                "report does not belong to authorization"
            );
            value["result"] = serde_json::to_value(&r)?;
        }
    }
    if matches!(a.state.as_str(), "evaluating" | "submitting") && store.lock().is_ok() {
        value["state"] = json!("interrupted_outcome_unknown");
        value["message"]=json!("Worker stopped. Inspect existing report/wallet; never use a new request ID to replay it.");
    }
    Ok(value)
}
pub fn revoke(c: &Config, id: &str) -> Result<Value> {
    let store = Store::open(c, id)?;
    if !store.dir.join("revoked.json").try_exists()? {
        write(
            &store.dir.join("revoked.json"),
            &json!({"at":Utc::now()}),
            false,
        )?;
    }
    Ok(
        json!({"authorization_id":id,"revoked":true,"message":"Future submissions are blocked; an already dispatched transaction cannot be recalled."}),
    )
}
pub async fn refresh(c: Config, id: String, request_id: String) -> Result<Value> {
    let store = Store::open(&c, &id)?;
    let _lock = store.lock()?;
    execution(&c, &id, &request_id)?;
    let path = store.path(&request_id, "attempt.json")?;
    let mut attempt: Attempt = read(&path)?;
    let was_stopped = attempt.state != "completed";
    let report = agentic::track(c.clone(), &store.path(&request_id, "report.json")?).await?;
    attempt.state = report.state;
    attempt.message = report.error;
    attempt.updated_at = Utc::now();
    write(&path, &attempt, true)?;
    if was_stopped || attempt.state != "completed" {
        store.halt(
            "Interrupted or exceptional execution was refreshed; operator review still required",
        )?;
    }
    // Keep the halt sticky even when read-only reconciliation succeeds.
    execution(&c, &id, &request_id)
}

pub fn list(c: &Config) -> Result<Value> {
    let base = c.state_dir.join("autonomy");
    if !base.try_exists()? {
        return Ok(json!({"authorizations":[]}));
    }
    ensure!(
        !fs::symlink_metadata(&base)?.file_type().is_symlink(),
        "invalid authorization directory"
    );
    let mut entries = vec![];
    for entry in fs::read_dir(base)?.take(257) {
        ensure!(
            entries.len() < 256,
            "too many authorizations; inspect by ID"
        );
        let path = entry?.path();
        ensure!(
            path.is_dir() && !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "invalid authorization directory entry"
        );
        if !path.join("policy.json").try_exists()? {
            continue;
        }
        let m: Mandate = read(&path.join("policy.json"))?;
        entries.push(status(c, &m.authorization_id)?);
    }
    Ok(json!({"authorizations":entries}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(
        max_orders: u32,
        total: &str,
        cooldown: u32,
    ) -> (tempfile::TempDir, Config, Snapshot, Intent) {
        let d = tempfile::tempdir().unwrap();
        let mut v: Value =
            serde_json::from_str(include_str!("../examples/agentic-config.json")).unwrap();
        let executable = d.path().join("never-execute");
        fs::write(&executable, "unused").unwrap();
        v["executable"] = json!(executable);
        v["wallet_address"] = json!(format!("0x{}", "1".repeat(40)));
        v["state_dir"] = json!(d.path().join("state"));
        let path = d.path().join("config.json");
        fs::write(&path, v.to_string()).unwrap();
        let c = Config::read(&path).unwrap();
        let snapshot = Snapshot::new(strategy::TEMPLATE, Default::default()).unwrap();
        let doc = postman_flow::parse_flow_yaml(strategy::TEMPLATE).unwrap();
        let intent: Intent = serde_json::from_value(
            doc.flow
                .inputs
                .iter()
                .find(|i| i.name == "intent")
                .unwrap()
                .default
                .clone()
                .unwrap(),
        )
        .unwrap();
        let limits = Limits {
            max_orders,
            max_total_sell_amount: total.into(),
            valid_for_minutes: 60,
            cooldown_seconds: cooldown,
        };
        create(
            c.clone(),
            "test".into(),
            snapshot.clone(),
            intent.clone(),
            limits,
        )
        .unwrap();
        (d, c, snapshot, intent)
    }
    #[test]
    fn reservations_are_durable_and_both_order_and_total_caps_apply() {
        for (max, total, allowed) in [(2, "18", 2), (5, "6", 1)] {
            let (_d, c, s, i) = fixture(max, total, 0);
            for n in 0..allowed {
                let mut p = claim(&c, "test", &format!("r{n}")).unwrap().unwrap();
                p.reserve(&c, &i, &s.binding().unwrap()).unwrap();
                assert!(p.reserve(&c, &i, &s.binding().unwrap()).is_err());
                p.finish("completed", None).unwrap();
            }
            let state = status(&c, "test").unwrap();
            assert_eq!(state["reserved_orders"], allowed);
            assert!(!state["eligible"].as_bool().unwrap());
            assert!(claim(&c, "test", "over-budget").is_err());
            assert!(claim(&c, "test", "r0").unwrap().is_none());
        }
    }
    #[test]
    fn revocation_and_changed_intent_are_rechecked_at_submission() {
        let (_d, c, s, i) = fixture(3, "18", 0);
        let mut p = claim(&c, "test", "one").unwrap().unwrap();
        let mut other = i.clone();
        other.amount = "5".into();
        assert!(p.reserve(&c, &other, &s.binding().unwrap()).is_err());
        revoke(&c, "test").unwrap();
        assert!(p.reserve(&c, &i, &s.binding().unwrap()).is_err());
        assert!(p.attempt.reserved_amount.is_none());
    }
    #[test]
    fn exclusive_claims_and_interruption_never_become_retry_permissions() {
        let (_d, c, _s, _i) = fixture(3, "18", 0);
        let p = claim(&c, "test", "one").unwrap().unwrap();
        assert!(claim(&c, "test", "one").unwrap().is_none());
        assert!(claim(&c, "test", "two").is_err());
        drop(p);
        assert_eq!(
            execution(&c, "test", "one").unwrap()["state"],
            "interrupted_outcome_unknown"
        );
        assert_eq!(status(&c, "test").unwrap()["eligible"], false);
        assert!(claim(&c, "test", "two").is_err());
    }
    #[test]
    fn unknown_submission_keeps_budget_and_cannot_be_reset_by_new_id() {
        let (_d, c, s, i) = fixture(3, "18", 0);
        let mut p = claim(&c, "test", "one").unwrap().unwrap();
        p.reserve(&c, &i, &s.binding().unwrap()).unwrap();
        drop(p);
        assert_eq!(status(&c, "test").unwrap()["reserved_sell_amount"], "6");
        assert!(claim(&c, "test", "two").is_err());
        assert!(create(
            c.clone(),
            "test".into(),
            s,
            i,
            Limits {
                max_orders: 9,
                max_total_sell_amount: "54".into(),
                valid_for_minutes: 60,
                cooldown_seconds: 0
            }
        )
        .is_err());
        assert_eq!(status(&c, "test").unwrap()["reserved_sell_amount"], "6");
    }
    #[test]
    fn expiry_cooldown_and_config_changes_fail_closed() {
        let (_d, c, s, i) = fixture(3, "18", 300);
        let mut p = claim(&c, "test", "one").unwrap().unwrap();
        p.reserve(&c, &i, &s.binding().unwrap()).unwrap();
        p.finish("completed", None).unwrap();
        drop(p);
        assert!(claim(&c, "test", "two")
            .err()
            .unwrap()
            .to_string()
            .contains("cooldown"));
        let mut other = c.clone();
        other.max_slippage_bps = 49;
        assert!(claim(&other, "test", "two").is_err());
        let store = Store::open(&c, "test").unwrap();
        let mut m = store.mandate;
        m.expires_at = Utc::now() - chrono::Duration::seconds(1);
        write(&store.dir.join("policy.json"), &m, true).unwrap();
        assert!(claim(&c, "test", "two").is_err());
    }
    #[test]
    fn corrupted_ledger_and_changed_snapshot_cannot_free_budget() {
        let (_d, c, s, i) = fixture(3, "18", 0);
        let mut p = claim(&c, "test", "one").unwrap().unwrap();
        p.reserve(&c, &i, &s.binding().unwrap()).unwrap();
        p.finish("completed", None).unwrap();
        let attempt_path = p.store.path("one", "attempt.json").unwrap();
        drop(p);
        fs::write(attempt_path, b"{}").unwrap();
        assert!(status(&c, "test").is_err());
        assert!(claim(&c, "test", "two").is_err());
        let (_d, c, s, i) = fixture(3, "18", 0);
        let mut p = claim(&c, "test", "one").unwrap().unwrap();
        let binding = s.binding().unwrap();
        fs::write(
            c.state_dir
                .join("strategy-snapshots")
                .join(format!("{}.json", binding.snapshot_sha256)),
            b"{}",
        )
        .unwrap();
        assert!(p.reserve(&c, &i, &binding).is_err());
    }
}
