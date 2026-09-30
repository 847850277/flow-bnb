//! Isolated, explicitly labelled synthetic quotes and receipts. No live adapter is used.
use super::*;
use postman_http::{
    request::{Request, RequestBody, RequestOptions},
    response::HttpResponse,
    HttpError, HttpTransport,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Frame {
    signal_receive: String,
    exit_receive: String,
}
#[derive(Clone)]
struct Quotes {
    frame: Arc<Mutex<Frame>>,
}
impl HttpTransport for Quotes {
    async fn execute(&self, r: Request, _: RequestOptions) -> Result<HttpResponse, HttpError> {
        let frame = self.frame.lock().unwrap().clone();
        let RequestBody::Json(body) = r.body else {
            return Err(HttpError::invalid_request("replay expects a quote body"));
        };
        let body: Value = serde_json::from_str(&body)
            .map_err(|_| HttpError::invalid_request("invalid replay body"))?;
        let (intent, receive, from, to) = match r.url.as_str() {
            "https://flow-bnb.invalid/strategy/observe-quote" => {
                (&body["intent"], frame.signal_receive, "USDT", "NVDAon")
            }
            "https://flow-bnb.invalid/strategy/quote" => {
                (&body, frame.exit_receive, "AAPLon", "USDT")
            }
            _ => {
                return Err(HttpError::invalid_request(
                    "replay does not access live market data or arbitrary endpoints",
                ))
            }
        };
        Ok(HttpResponse::new(
            200,
            vec![],
            json!({"success":true,"data":{
                "fromCoinSymbol":from,"toCoinSymbol":to,"fromCoinAmount":intent["amount"],
                "toCoinAmount":receive,"slippage":0.005
            }})
            .to_string(),
        ))
    }
}
struct Replay {
    config: Config,
    quotes: Quotes,
    orders: Mutex<BTreeMap<String, Value>>,
    actual_entry_quantity: String,
    sell_fraction: bool,
    unknown_order: bool,
}
impl Backend for Replay {
    fn environment(&self) -> &'static str {
        "simulation"
    }
    async fn evaluate(&self, snapshot: &Snapshot) -> Result<RunReport> {
        strategy::run_with(&self.config, snapshot, self.quotes.clone()).await
    }
    async fn order(&self, snapshot: &Snapshot, id: &str, execute: bool) -> Result<Value> {
        if let Some(v) = self.orders.lock().unwrap().get(id).cloned() {
            return Ok(v);
        }
        if !execute {
            return Ok(json!({"state":"awaiting_execution"}));
        }
        let run = self.evaluate(snapshot).await?;
        ensure!(run.success, "replay strategy failed");
        let d = run.decision.context("replay decision missing")?;
        if !d.triggered {
            return Ok(json!({"state":"not_triggered"}));
        }
        let is_buy = id.ends_with(".buy");
        let (sold, received) = if is_buy {
            (d.intent.amount.clone(), self.actual_entry_quantity.clone())
        } else {
            (
                if self.sell_fraction {
                    "0.014".into()
                } else {
                    d.intent.amount.clone()
                },
                self.quotes.frame.lock().unwrap().exit_receive.clone(),
            )
        };
        let state = if self.unknown_order {
            "claimed_outcome_unknown"
        } else if self.sell_fraction && !is_buy {
            "settled_with_discrepancy"
        } else {
            "completed"
        };
        let v = json!({"state":state,"intent_id":id,"intent":d.intent,"simulation":true,
            "result":{"order_id":if self.unknown_order {Value::Null} else {json!(format!("SIMULATED-{id}"))},
                "tx_hash":null,"settlement":{"source":"simulated_receipt_transfer_logs","sold":sold,"received":received}}});
        self.orders.lock().unwrap().insert(id.into(), v.clone());
        Ok(v)
    }
}
fn fixture(base: &Snapshot) -> Result<(tempfile::TempDir, Replay)> {
    let dir = tempfile::tempdir()?;
    let entry = entry_intent(base)?;
    let config = Config {
        executable: dir.path().join("NO-LIVE-WALLET"),
        wallet_address: format!("0x{}", "1".repeat(40)),
        rpc_url: "http://127.0.0.1:1".into(),
        state_dir: dir.path().join("state"),
        max_slippage_bps: 50,
        trace: Default::default(),
        tokens: vec![
            agentic::TokenRule {
                address: entry.from_token,
                symbol: "USDT".into(),
                decimals: 18,
                max_sell_amount: "100".into(),
            },
            agentic::TokenRule {
                address: entry.to_token,
                symbol: "AAPLon".into(),
                decimals: 18,
                max_sell_amount: "100".into(),
            },
        ],
    };
    let backend = Replay {
        config,
        quotes: Quotes {
            frame: Arc::new(Mutex::new(Frame {
                signal_receive: "0.0245".into(),
                exit_receive: "5".into(),
            })),
        },
        orders: Default::default(),
        actual_entry_quantity: "0.015".into(),
        sell_fraction: false,
        unknown_order: false,
    };
    Ok((dir, backend))
}

pub async fn run(base: Snapshot) -> Result<Value> {
    let (_dir, backend) = fixture(&base)?;
    let entry_amount = agentic::units(&entry_intent(&base)?.amount, 18)?;
    let mut events = Vec::new();
    let mut final_state = Value::Null;
    // Frames describe synthetic inputs, not predetermined actions. Edited YAML
    // thresholds may legitimately leave the cycle waiting or holding.
    for (label, signal, exit_percent) in [
        ("Record startup baseline", "0.0245", 100u32),
        ("Evaluate the first synthetic signal change", "0.02475", 100),
        ("Synthetic signal price reaches -2%", "0.025", 100),
        ("Evaluate the first synthetic exit quote", "0.025", 101),
        ("Evaluate the second synthetic exit quote", "0.025", 102),
        ("Repeat the previous inputs", "0.025", 102),
    ] {
        let exit = agentic::decimal(&(&entry_amount * exit_percent / 100u32), 18);
        *backend.quotes.frame.lock().unwrap() = Frame {
            signal_receive: signal.into(),
            exit_receive: exit.clone(),
        };
        final_state = advance(&backend.config, &base, "replay", true, &backend).await?;
        events.push(json!({"label":label,"phase":final_state["phase"],
            "synthetic_quotes":{"signal_receive":signal,"exit_receive":exit},
            "outputs":final_state["last_evaluation"]["outputs"],"steps":final_state["last_evaluation"]["steps"],
            "simulated_orders":backend.orders.lock().unwrap().len(),
            "entry_cost":final_state["entry_cost"],"position_quantity":final_state["position_quantity"]}));
    }
    Ok(
        json!({"mode":"simulation","live_transactions":false,"snapshot_sha256":base.binding()?.snapshot_sha256,
        "data_basis":"Synthetic fixed-size quotes and synthetic receipts, not historical or live market data.",
        "events":events,"final_phase":final_state["phase"],"summary":final_state["summary"],
        "simulated_orders":backend.orders.lock().unwrap().len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Snapshot {
        Snapshot::new(TEMPLATE, BTreeMap::new()).unwrap()
    }
    fn prices(b: &Replay, signal: &str, exit: &str) {
        *b.quotes.frame.lock().unwrap() = Frame {
            signal_receive: signal.into(),
            exit_receive: exit.into(),
        };
    }
    async fn initialize(b: &Replay, s: &Snapshot) {
        assert_eq!(
            advance(&b.config, s, "test", true, b).await.unwrap()["phase"],
            "waiting_entry"
        );
    }
    #[tokio::test]
    async fn complete_round_trip_runs_yaml_and_submits_each_leg_once() {
        let r = run(base()).await.unwrap();
        assert_eq!(r["final_phase"], "completed");
        assert_eq!(r["simulated_orders"], 2);
        assert_eq!(r["summary"]["exit_received"], "5.1");
        assert_eq!(r["summary"]["remaining_position"], "0");
        assert_eq!(r["events"][1]["phase"], "waiting_entry");
        assert_eq!(r["events"][2]["phase"], "holding");
        assert_eq!(r["events"][3]["phase"], "holding");
        assert_eq!(r["events"][2]["outputs"]["change_percent"], "-2");
        assert_eq!(r["events"][4]["outputs"]["change_percent"], "2");
    }
    #[tokio::test]
    async fn replay_evaluates_edited_amounts_and_thresholds_without_forcing_completion() {
        let mut entry = entry_intent(&base()).unwrap();
        entry.amount = "10".into();
        let changed = Snapshot::new(
            TEMPLATE,
            BTreeMap::from([("entry_intent".into(), serde_json::to_value(entry).unwrap())]),
        )
        .unwrap();
        let r = run(changed).await.unwrap();
        assert_eq!(r["final_phase"], "completed");
        assert_eq!(r["summary"]["exit_received"], "10.2");
        for (input, threshold, phase, count) in [
            ("entry_change_bps", -300, "waiting_entry", 0),
            ("take_profit_bps", 300, "holding", 1),
        ] {
            let changed =
                Snapshot::new(TEMPLATE, BTreeMap::from([(input.into(), json!(threshold))]))
                    .unwrap();
            let r = run(changed).await.unwrap();
            assert_eq!(r["final_phase"], phase);
            assert_eq!(r["simulated_orders"], count);
            assert!(r["summary"].is_null());
        }
    }
    #[tokio::test]
    async fn preview_does_not_submit_and_actual_inventory_drives_exit() {
        let s = base();
        let (_d, b) = fixture(&s).unwrap();
        initialize(&b, &s).await;
        prices(&b, "0.025", "5");
        let preview = advance(&b.config, &s, "test", false, &b).await.unwrap();
        assert_eq!(preview["ready_to_submit"], true);
        assert!(b.orders.lock().unwrap().is_empty());
        let holding = advance(&b.config, &s, "test", true, &b).await.unwrap();
        assert_eq!(holding["position_quantity"], "0.015");
        prices(&b, "0.030", "5.10");
        advance(&b.config, &s, "test", true, &b).await.unwrap();
        for (id, order) in b.orders.lock().unwrap().iter() {
            if id.ends_with(".sell") {
                assert_eq!(order["intent"]["amount"], "0.015");
            }
        }
        assert_eq!(b.orders.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn restart_after_submission_recovers_same_order_identity() {
        let s = base();
        let (_d, b) = fixture(&s).unwrap();
        initialize(&b, &s).await;
        prices(&b, "0.025", "5");
        let store = Store::open(&b.config, "test").unwrap();
        let mut state = store.read().unwrap().unwrap();
        state.phase = "buying".into();
        store.save(&mut state).unwrap();
        b.order(
            &contextual(&s, &state).unwrap(),
            &format!("cycle-{}.buy", key("test").unwrap()),
            true,
        )
        .await
        .unwrap();
        drop(store); // Simulate termination before the cycle records the entry receipt.
        assert_eq!(
            advance(&b.config, &s, "test", true, &b).await.unwrap()["phase"],
            "holding"
        );
        assert_eq!(b.orders.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn unknown_submission_stops_cycle_without_replaying() {
        let s = base();
        let (_d, mut b) = fixture(&s).unwrap();
        b.unknown_order = true;
        initialize(&b, &s).await;
        prices(&b, "0.025", "5");
        assert_eq!(
            advance(&b.config, &s, "test", true, &b).await.unwrap()["phase"],
            "needs_attention"
        );
        advance(&b.config, &s, "test", true, &b).await.unwrap();
        assert_eq!(b.orders.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn partial_exit_is_reported_without_selling_the_remainder() {
        let s = base();
        let (_d, mut b) = fixture(&s).unwrap();
        b.sell_fraction = true;
        initialize(&b, &s).await;
        prices(&b, "0.025", "5");
        advance(&b.config, &s, "test", true, &b).await.unwrap();
        prices(&b, "0.025", "5.10");
        let r = advance(&b.config, &s, "test", true, &b).await.unwrap();
        assert_eq!(r["phase"], "completed_with_discrepancy");
        assert_eq!(r["summary"]["remaining_position"], "0.001");
        advance(&b.config, &s, "test", true, &b).await.unwrap();
        assert_eq!(b.orders.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn state_source_and_environment_cannot_be_switched_during_a_cycle() {
        let s = base();
        let (_d, b) = fixture(&s).unwrap();
        initialize(&b, &s).await;
        let changed = Snapshot::new(
            &s.yaml,
            BTreeMap::from([("take_profit_bps".into(), json!(300))]),
        )
        .unwrap();
        assert!(advance(&b.config, &changed, "test", true, &b)
            .await
            .is_err());
        assert!(step(b.config.clone(), s.clone(), "test", true)
            .await
            .is_err());
        let injected = Snapshot::new(
            &s.yaml,
            BTreeMap::from([(CONTEXT.into(), json!({"phase":"holding"}))]),
        )
        .unwrap();
        assert!(advance(&b.config, &injected, "other", true, &b)
            .await
            .is_err());
        assert!(b.orders.lock().unwrap().is_empty());
    }
}
