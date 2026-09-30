//! Native execution with durable request deduplication. Explicit execution calls
//! start work; opening an inbox never drains historical requests.
use crate::{
    agentic,
    handoff::{Claim, TypedInbox},
};
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

#[derive(Clone)]
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

    /// Claim synchronously before returning, then execute in the MCP runtime.
    pub fn start(&self, request_id: String, intent: agentic::Intent) -> Result<Value> {
        let queued = self.enqueue(request_id, intent)?;
        self.start_id(queued["intent_id"].as_str().context("missing intent ID")?)
    }

    fn start_id(&self, id: &str) -> Result<Value> {
        let Some(claim) = self.claim(id)? else {
            return self.status(id);
        };
        let status = self.status(id)?;
        let inbox = self.clone();
        tokio::spawn(async move {
            if let Err(e) = inbox.execute_claim(claim).await {
                tracing::error!("Native execution stopped: {e:#}");
            }
        });
        Ok(status)
    }

    /// Evaluate the frozen strategy, then execute a triggered decision. CLI callers
    /// await completion; MCP callers keep the server running and poll the returned ID.
    pub async fn execute_strategy(
        &self,
        request_id: String,
        snapshot: crate::strategy::Snapshot,
        background: bool,
    ) -> Result<Value> {
        let evaluation = self.enqueue_strategy(request_id, snapshot).await?;
        let Some(id) = evaluation["intent_id"].as_str() else {
            return Ok(evaluation);
        };
        let mut result = if background {
            self.start_id(id)?
        } else {
            self.execute(id).await?
        };
        if let Some(run) = evaluation.get("strategy_run") {
            result["strategy_run"] = run.clone();
        }
        Ok(result)
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
            "wallet configuration changed since this request was saved"
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
            "can_cancel":journal.state=="awaiting_operator", "retry_execution":false,
            "next_action":"Query get_agentic_execution with this intent_id. Keep MCP running while executing; reuse the original request_id on transport retries."
        });
        if journal.state == "awaiting_operator" {
            value["state"] = json!("queued");
            value["message"] = json!("Saved request has not started; no transaction submitted.");
            value["next_action"] = json!("Repeat the original execution request with the same request_id to start it, or cancel this intent. Old requests are not started automatically.");
        }
        let report = match self.read_report(id) {
            Ok(report) => report,
            Err(e) => {
                value["state"] = json!("needs_attention");
                value["can_cancel"] = json!(false);
                value["message"] = json!("Execution evidence is unreadable or does not match the intent. Inspect existing records; never resubmit.");
                value["result"] = json!({"error":e.to_string()});
                value["next_action"] =
                    json!("Inspect existing order records; this request cannot be replayed.");
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
                "tx_hash":r.order["txHash"], "settlement":r.settlement,
                "token_audit":r.token_audit, "error":r.error
            });
            value["message"] = json!(match state {
                "completed" => "Order and on-chain transfers verified.",
                "settled_with_discrepancy" => "On-chain trade verified with an amount discrepancy. Compare actual transfers in the result; this does not lock the wallet or authorize another order.",
                "not_triggered" => "The strategy condition no longer holds. No order was submitted.",
                "cancelled" | "blocked" | "quote_changed" | "strategy_blocked" | "not_submitted" => "No order submitted by this attempt. The intent remains claimed and cannot replay.",
                _ => "Inspect the existing order; use refresh_agentic_execution when an order ID is available. Never resubmit."
            });
            value["next_action"] = json!(match state {
                "completed" => "Execution finished. Show the order, transaction hash and received amount.",
                "settled_with_discrepancy" => "Execution finished. Show the actual transfers and discrepancy; do not automatically retry or sell the difference.",
                "not_triggered" | "cancelled" | "blocked" | "quote_changed" | "strategy_blocked" | "not_submitted" => "Show why this attempt did not submit. No further polling is needed.",
                _ => "Inspect the existing order. Use refresh_agentic_execution if an order ID is available; never replay this request."
            });
        } else if self.report_path(id)?.try_exists()? {
            value["state"] = json!("executing");
            value["can_cancel"] = json!(false);
            value["message"] = json!(
                "Execution or read-only verification is in progress. Query this intent again."
            );
        } else if !matches!(journal.state.as_str(), "awaiting_operator" | "cancelled") {
            let journal = fs::File::open(self.directory.join(format!("{id}.journal.jsonl")))?;
            if journal.try_lock_shared().is_ok() {
                value["state"] = json!("claimed_outcome_unknown");
                value["message"] = json!("Execution stopped and no report is available. Inspect wallet and journal; never replay this request.");
                value["next_action"] =
                    json!("Inspect existing order records; this request cannot be replayed.");
            }
        }
        Ok(value)
    }

    pub fn cancel(&self, id: &str) -> Result<Value> {
        self.queue.cancel(id)?;
        self.status(id)
    }

    pub(crate) fn status_for_request(&self, request_id: &str) -> Result<Option<Value>> {
        let id = Self::request_key(request_id)?;
        if !self
            .directory
            .join(format!("{id}.intent.json"))
            .try_exists()?
        {
            return Ok(None);
        }
        self.status(&id).map(Some)
    }

    pub fn pending(&self) -> Result<Vec<String>> {
        Ok(self.queue.pending()?.into_iter().map(|i| i.id).collect())
    }

    /// A durable claim deduplicates only this request; it never locks the wallet.
    fn claim(&self, id: &str) -> Result<Option<Claim<Request>>> {
        let request = self.queue.intent(id)?.request;
        self.check_binding(&request)?;
        if self.queue.status(id)?.state != "awaiting_operator" {
            return Ok(None);
        }
        if let Some(binding) = &request.strategy {
            binding.load(&self.config)?;
        }
        let mut claim = match self.queue.claim(id) {
            Ok(claim) => claim,
            // A concurrent request or cancellation may have claimed the ID.
            Err(_) if self.queue.status(id)?.state != "awaiting_operator" => return Ok(None),
            Err(e) => return Err(e),
        };
        claim.record(
            "executing",
            "Preparing and executing the requested trade. This request cannot submit twice.",
        )?;
        Ok(Some(claim))
    }

    /// Foreground compatibility entry point for an explicitly selected saved request.
    pub async fn execute(&self, id: &str) -> Result<Value> {
        if let Some(claim) = self.claim(id)? {
            self.execute_claim(claim).await?;
        }
        self.status(id)
    }

    async fn execute_claim(&self, mut claim: Claim<Request>) -> Result<()> {
        let request = claim.intent.request.clone();
        match agentic::run_with_strategy(
            self.config.clone(),
            request.intent,
            &self.report_path(&claim.intent.id)?,
            true,
            request.strategy,
        )
        .await
        {
            Ok(r) => claim.record(
                &r.state,
                "Execution finished; inspect the bound native report.",
            )?,
            Err(e) => claim.record(
                "needs_attention",
                &format!("Execution stopped: {e}. Inspect existing evidence; do not resubmit."),
            )?,
        }
        Ok(())
    }

    /// Resumes existing evidence only. This cannot call native swap or create a claim.
    pub async fn refresh(&self, id: &str) -> Result<Value> {
        let request = self.queue.intent(id)?.request;
        self.check_binding(&request)?;
        let report = self
            .read_report(id)?
            .context("no readable report; execution may still be active")?;
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
    async fn config_change_blocks_before_claiming_or_wallet_io() {
        let (_tmp, mut c, i) = setup();
        let inbox = Inbox::open(c.clone()).unwrap();
        let queued = inbox.enqueue("request-2".into(), i).unwrap();
        let id = queued["intent_id"].as_str().unwrap();
        c.max_slippage_bps = 40;
        let changed = Inbox::open(c.clone()).unwrap();
        assert!(changed
            .execute(id)
            .await
            .unwrap_err()
            .to_string()
            .contains("configuration changed"));
        assert_eq!(inbox.status(id).unwrap()["state"], "queued");
        assert!(inbox.refresh(id).await.is_err());
    }

    #[test]
    fn report_binding_symlinks_and_inflight_writes_are_not_trusted_as_completed() {
        let (tmp, c, i) = setup();
        let inbox = Inbox::open(c).unwrap();
        let queued = inbox.enqueue("request-3".into(), i.clone()).unwrap();
        let id = queued["intent_id"].as_str().unwrap();
        let mut claim = inbox.queue.claim(id).unwrap();
        claim.record("executing", "testing").unwrap();
        let path = inbox.report_path(id).unwrap();
        let file = fs::File::create(&path).unwrap();
        file.lock().unwrap();
        assert_eq!(inbox.status(id).unwrap()["state"], "executing");
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
        claim.record("executing", "fixture operator").unwrap();
        assert_eq!(inbox.status(id).unwrap()["state"], "executing");
        drop(claim);
        let status = inbox.status(id).unwrap();
        assert_eq!(status["state"], "claimed_outcome_unknown");
        assert_eq!(status["retry_execution"], false);
        assert!(inbox.pending().unwrap().is_empty());
        assert!(inbox.queue.claim(id).is_err());
    }
}
