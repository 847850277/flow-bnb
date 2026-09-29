//! Native-order handoff: MCP writes intents, an operator terminal executes them.
//! A durable claim is never reset; refreshing a report is strictly read-only.
use crate::{agentic, handoff::TypedInbox};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub intent: agentic::Intent,
    pub wallet_address: String,
    pub config_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<crate::strategy::Binding>,
}

pub struct Inbox {
    config: agentic::Config,
    directory: PathBuf,
    queue: TypedInbox<Request>,
}

impl Inbox {
    pub fn open(config: agentic::Config) -> Result<Self> {
        let directory = config.state_dir.join("native-handoff");
        let queue = TypedInbox::open(&directory)?;
        Ok(Self {
            config,
            directory,
            queue,
        })
    }

    /// Client-provided retry key deduplicates network retries of the same request.
    pub fn enqueue(&self, request_id: String, intent: agentic::Intent) -> Result<Value> {
        self.enqueue_bound(request_id, intent, None)
    }

    fn request_key(request_id: &str) -> Result<String> {
        ensure!(
            !request_id.is_empty()
                && request_id.len() <= 128
                && request_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "request_id must contain 1..128 ASCII letters, digits, dots, underscores or hyphens"
        );
        Ok(format!(
            "{:x}",
            Sha256::digest(format!("agentic-handoff-v1:{request_id}"))
        ))
    }

    fn enqueue_bound(
        &self,
        request_id: String,
        intent: agentic::Intent,
        strategy: Option<crate::strategy::Binding>,
    ) -> Result<Value> {
        self.config.rules(&intent)?;
        let id = Self::request_key(&request_id)?;
        self.queue.enqueue_with_id(
            Request {
                request_id,
                intent,
                wallet_address: self.config.wallet_address.clone(),
                config_sha256: agentic::digest(&self.config)?,
                strategy,
            },
            id.clone(),
        )?;
        self.status(&id)
    }

    /// Evaluate once and queue only a successful, triggered decision. Retries return the original intent.
    pub async fn enqueue_strategy(
        &self,
        request_id: String,
        snapshot: crate::strategy::Snapshot,
    ) -> Result<Value> {
        let id = Self::request_key(&request_id)?;
        let binding = snapshot.binding()?;
        if self
            .directory
            .join(format!("{id}.intent.json"))
            .try_exists()?
        {
            let existing = self.queue.intent(&id)?.request;
            self.check_binding(&existing)?;
            ensure!(existing.strategy.as_ref().is_some_and(|s| s.snapshot_sha256 == binding.snapshot_sha256),
                "request ID already belongs to another strategy or intent; query the existing intent");
            return self.status(&id);
        }
        let report = crate::strategy::run(&self.config, &snapshot).await?;
        self.finish_strategy(request_id, snapshot, report)
    }

    fn finish_strategy(
        &self,
        request_id: String,
        snapshot: crate::strategy::Snapshot,
        report: crate::strategy::RunReport,
    ) -> Result<Value> {
        if !report.success || !report.decision.as_ref().is_some_and(|d| d.triggered) {
            return Ok(
                json!({"state":if report.success {"not_triggered"} else {"strategy_failed"},
                "queued":false,"strategy_run":report}),
            );
        }
        let intent = report
            .decision
            .as_ref()
            .context("missing strategy decision")?
            .intent
            .clone();
        let binding = snapshot.persist(&self.config)?;
        let mut result = self.enqueue_bound(request_id, intent, Some(binding))?;
        result["queued"] = json!(true);
        result["strategy_run"] = serde_json::to_value(report)?;
        Ok(result)
    }

    fn report_path(&self, id: &str) -> Result<PathBuf> {
        // Validate the ID and the persisted intent before constructing any path.
        self.queue.intent(id)?;
        Ok(self.directory.join(format!("{id}.agentic-report.json")))
    }

    fn check_binding(&self, request: &Request) -> Result<()> {
        ensure!(
            request
                .wallet_address
                .eq_ignore_ascii_case(&self.config.wallet_address)
                && request.config_sha256 == agentic::digest(&self.config)?,
            "operator configuration changed since enqueue; review and create a new request"
        );
        self.config.rules(&request.intent)?;
        Ok(())
    }

    fn validate_report(&self, request: &Request, report: &agentic::Report) -> Result<()> {
        ensure!(
            report.schema_version == 1
                && report.backend == "agentic_wallet"
                && report
                    .wallet_address
                    .eq_ignore_ascii_case(&request.wallet_address)
                && report.config_sha256 == request.config_sha256
                && serde_json::to_value(&report.intent)? == serde_json::to_value(&request.intent)?,
            "report does not belong to this queued intent"
        );
        Ok(())
    }

    fn read_report(&self, id: &str) -> Result<Option<agentic::Report>> {
        let path = self.report_path(id)?;
        if !path.try_exists()? {
            return Ok(None);
        }
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 1_048_576,
            "invalid execution report file"
        );
        let file = fs::File::open(path)?;
        // run/track hold an exclusive lock, so no partially rewritten JSON is read.
        if file.try_lock_shared().is_err() {
            return Ok(None);
        }
        let report: agentic::Report = serde_json::from_reader(&file)
            .context("execution report incomplete; inspect wallet, never resubmit")?;
        self.validate_report(&self.queue.intent(id)?.request, &report)?;
        Ok(Some(report))
    }

    /// Reads local evidence only; never invokes the wallet, network or an executor.
    pub fn status(&self, id: &str) -> Result<Value> {
        let queued = self.queue.intent(id)?;
        let journal = self.queue.status(id)?;
        let mut value = json!({
            "intent_id":id, "request_id":queued.request.request_id,
            "backend":"agentic_wallet", "state":journal.state,
            "created_at":queued.created_at, "updated_at":journal.updated_at,
            "intent":queued.request.intent, "wallet_address":queued.request.wallet_address,
            "strategy":queued.request.strategy,
            "message":journal.message, "result":null,
            "operator_message":journal.message,
            "can_cancel":journal.state=="awaiting_operator", "retry_execution":false,
            "next_action":"An operator runs agentic-operator with this intent_id, or --watch. Query this ID for results; never replace it to retry a trade."
        });
        let report = match self.read_report(id) {
            Ok(report) => report,
            Err(e) => {
                value["state"] = json!("needs_attention");
                value["can_cancel"] = json!(false);
                value["message"] = json!("Execution evidence is unreadable or does not match the intent. Inspect existing records; never resubmit.");
                value["result"] = json!({"error":e.to_string()});
                return Ok(value);
            }
        };
        if let Some(r) = report {
            // A stopped ready report is not an execution capability or a retryable claim.
            let state = if r.state == "ready" {
                "not_submitted"
            } else {
                &r.state
            };
            value["state"] = json!(state);
            value["can_cancel"] = json!(false);
            value["result"] = json!({
                "order_id":r.order_id, "order_status":r.order["status"],
                "tx_hash":r.order["txHash"], "settlement":r.settlement, "error":r.error
            });
            value["message"] = json!(match state {
                "completed" => "Order and on-chain transfers verified.",
                "settled_with_discrepancy" => "On-chain trade verified with an amount discrepancy. Review required; wallet lock retained. Never resubmit or automatically sell the difference.",
                "cancelled" | "blocked" | "quote_changed" | "strategy_blocked" | "not_submitted" => "No order submitted by this attempt. The intent remains claimed and cannot replay.",
                _ => "Inspect the existing order; use refresh_agentic_execution when an order ID is available. Never resubmit."
            });
        } else if self.report_path(id)?.try_exists()? {
            value["state"] = json!("operator_active");
            value["can_cancel"] = json!(false);
            value["message"] = json!("Operator or read-only verifier holds the report lock. Query this intent again; do not resubmit.");
        } else if !matches!(journal.state.as_str(), "awaiting_operator" | "cancelled") {
            let journal = fs::File::open(self.directory.join(format!("{id}.journal.jsonl")))?;
            if journal.try_lock_shared().is_ok() {
                value["state"] = json!("claimed_outcome_unknown");
                value["message"] = json!("Operator is no longer active and no report is available. Inspect wallet and journal; never resubmit.");
            }
        }
        Ok(value)
    }

    pub fn cancel(&self, id: &str) -> Result<Value> {
        self.queue.cancel(id)?;
        self.status(id)
    }

    pub fn pending(&self) -> Result<Vec<String>> {
        Ok(self.queue.pending()?.into_iter().map(|i| i.id).collect())
    }

    /// Only the local operator CLI calls this; native run requires /dev/tty confirmation.
    pub async fn approve(&self, id: &str) -> Result<Value> {
        let request = self.queue.intent(id)?.request;
        self.check_binding(&request)?;
        let barrier = self.config.state_dir.join(format!(
            "agentic-{}.lock",
            self.config.wallet_address.to_lowercase()
        ));
        ensure!(
            !barrier.try_exists()?,
            "wallet submission lock exists; resolve previous order before claiming another intent"
        );
        if let Some(binding) = &request.strategy {
            let snapshot = binding.load(&self.config)?;
            println!(
                "Strategy snapshot {} (inputs and flow):\n{}",
                binding.snapshot_sha256,
                serde_json::to_string_pretty(&snapshot)?
            );
        }
        let mut claim = self.queue.claim(id)?;
        claim.record(
            "operator_active",
            "Operator is preparing, reviewing or tracking. This intent cannot be claimed again.",
        )?;
        println!("Reviewing queued Agentic Wallet intent: {id}");
        match agentic::run_with_strategy(
            self.config.clone(),
            request.intent,
            &self.report_path(id)?,
            true,
            request.strategy,
        )
        .await
        {
            Ok(r) => claim.record(
                &r.state,
                "Operator run finished; inspect the bound native report.",
            )?,
            Err(e) => claim.record(
                "needs_attention",
                &format!("Operator stopped: {e}. Inspect existing evidence; do not resubmit."),
            )?,
        }
        self.status(id)
    }

    /// Resumes existing evidence only. This cannot call native swap or create a claim.
    pub async fn refresh(&self, id: &str) -> Result<Value> {
        let request = self.queue.intent(id)?.request;
        self.check_binding(&request)?;
        let report = self
            .read_report(id)?
            .context("no readable report; operator may still be active")?;
        ensure!(
            report.order_id.is_some(),
            "no order ID; cannot refresh or resubmit automatically"
        );
        agentic::track(self.config.clone(), &self.report_path(id)?).await?;
        self.status(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn setup() -> (tempfile::TempDir, agentic::Config, agentic::Intent) {
        let tmp = tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let sell = format!("0x{}", "2".repeat(40));
        let buy = format!("0x{}", "3".repeat(40));
        let config = agentic::Config {
            executable: "/unused".into(),
            wallet_address: format!("0x{}", "1".repeat(40)),
            rpc_url: "http://127.0.0.1:1".into(),
            max_slippage_bps: 50,
            state_dir: tmp.path().to_path_buf(),
            trace: Default::default(),
            tokens: vec![
                agentic::TokenRule {
                    address: sell.clone(),
                    symbol: "SELL".into(),
                    decimals: 18,
                    max_sell_amount: "6".into(),
                },
                agentic::TokenRule {
                    address: buy.clone(),
                    symbol: "BUY".into(),
                    decimals: 18,
                    max_sell_amount: "6".into(),
                },
            ],
        };
        (
            tmp,
            config,
            agentic::Intent {
                from_token: sell,
                to_token: buy,
                amount: "6".into(),
                slippage_bps: 50,
            },
        )
    }

    #[test]
    fn retry_key_is_stable_but_cannot_change_intent_or_revive_cancelled_request() {
        let (_tmp, c, i) = setup();
        let inbox = Inbox::open(c.clone()).unwrap();
        let first = inbox.enqueue("request-1".into(), i.clone()).unwrap();
        let id = first["intent_id"].as_str().unwrap();
        assert_eq!(
            inbox.enqueue("request-1".into(), i.clone()).unwrap()["intent_id"],
            id
        );
        let mut changed = i.clone();
        changed.amount = "5".into();
        assert!(inbox.enqueue("request-1".into(), changed).is_err());
        assert_eq!(inbox.pending().unwrap(), vec![id]);
        assert_eq!(inbox.cancel(id).unwrap()["state"], "cancelled");
        assert!(inbox.pending().unwrap().is_empty());
        assert_eq!(
            Inbox::open(c)
                .unwrap()
                .enqueue("request-1".into(), i)
                .unwrap()["state"],
            "cancelled"
        );
        assert!(inbox.queue.claim(id).is_err());
        assert!(inbox.status("../escape").is_err());
    }

    #[tokio::test]
    async fn config_change_and_unresolved_wallet_block_before_claiming_or_wallet_io() {
        let (_tmp, mut c, i) = setup();
        let inbox = Inbox::open(c.clone()).unwrap();
        let queued = inbox.enqueue("request-2".into(), i).unwrap();
        let id = queued["intent_id"].as_str().unwrap();
        c.max_slippage_bps = 40;
        let changed = Inbox::open(c.clone()).unwrap();
        assert!(changed
            .approve(id)
            .await
            .unwrap_err()
            .to_string()
            .contains("configuration changed"));
        let barrier = c
            .state_dir
            .join(format!("agentic-{}.lock", c.wallet_address.to_lowercase()));
        fs::write(barrier, "previous unresolved order").unwrap();
        assert!(inbox
            .approve(id)
            .await
            .unwrap_err()
            .to_string()
            .contains("submission lock"));
        assert_eq!(inbox.status(id).unwrap()["state"], "awaiting_operator");
        assert!(inbox.refresh(id).await.is_err());
    }

    #[test]
    fn report_binding_symlinks_and_inflight_writes_are_not_trusted_as_completed() {
        let (tmp, c, i) = setup();
        let inbox = Inbox::open(c).unwrap();
        let queued = inbox.enqueue("request-3".into(), i.clone()).unwrap();
        let id = queued["intent_id"].as_str().unwrap();
        let mut claim = inbox.queue.claim(id).unwrap();
        claim.record("operator_active", "testing").unwrap();
        let path = inbox.report_path(id).unwrap();
        let file = fs::File::create(&path).unwrap();
        file.lock().unwrap();
        assert_eq!(inbox.status(id).unwrap()["state"], "operator_active");
        drop(file);
        // A truncated report must not be treated as an unclaimed/retryable request.
        assert_eq!(inbox.status(id).unwrap()["state"], "needs_attention");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "schema_version":1,"backend":"agentic_wallet","state":"completed","intent":i,
                "wallet_address":claim.intent.request.wallet_address,"config_sha256":"wrong",
                "prepared_at":"fixture","stages":[],"balances":null,"quote":null,
                "wallet_settings":null,"token_audit":null,"order_id":"123","order":null,
                "settlement":{},"error":null
            }))
            .unwrap(),
        )
        .unwrap();
        let invalid = inbox.status(id).unwrap();
        assert_eq!(invalid["state"], "needs_attention");
        assert_eq!(invalid["retry_execution"], false);
        assert!(invalid["result"]["error"]
            .as_str()
            .unwrap()
            .contains("does not belong"));
        fs::remove_file(&path).unwrap();
        fs::write(tmp.path().join("elsewhere"), "{}").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), &path).unwrap();
        assert_eq!(inbox.status(id).unwrap()["state"], "needs_attention");
    }

    #[test]
    fn interrupted_claim_without_report_is_unknown_and_never_returns_to_pending() {
        let (_tmp, c, i) = setup();
        let inbox = Inbox::open(c).unwrap();
        let queued = inbox.enqueue("interrupted".into(), i).unwrap();
        let id = queued["intent_id"].as_str().unwrap();
        let mut claim = inbox.queue.claim(id).unwrap();
        claim.record("operator_active", "fixture operator").unwrap();
        assert_eq!(inbox.status(id).unwrap()["state"], "operator_active");
        drop(claim);
        let status = inbox.status(id).unwrap();
        assert_eq!(status["state"], "claimed_outcome_unknown");
        assert_eq!(status["retry_execution"], false);
        assert!(inbox.pending().unwrap().is_empty());
        assert!(inbox.queue.claim(id).is_err());
    }
}
