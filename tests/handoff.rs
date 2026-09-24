#![cfg(unix)]
use flow_bnb::{handoff::Inbox, trade::TradeRequest};
use std::sync::{Arc, Barrier};
fn request() -> TradeRequest {
    TradeRequest {
        wallet_address: flow_bnb::demo::WALLET.into(),
        from_token_address: flow_bnb::demo::SELL.into(),
        to_token_address: flow_bnb::demo::BUY.into(),
        amount: "5000000000000000000".into(),
        slippage_bps: 50,
    }
}
#[test]
fn queue_survives_restart_but_claim_can_never_be_replayed() {
    let dir = private_directory();
    let inbox = Inbox::open(dir.path()).unwrap();
    let queued = inbox.enqueue(request()).unwrap();
    let id = queued.intent.id;
    assert_eq!(inbox.status(&id).unwrap().state, "awaiting_operator");
    let mut claim = inbox.claim(&id).unwrap();
    claim
        .record("handoff_started_outcome_unknown", "test")
        .unwrap();
    drop(claim);
    let reopened = Inbox::open(dir.path()).unwrap();
    assert_eq!(
        reopened.status(&id).unwrap().state,
        "handoff_started_outcome_unknown"
    );
    assert!(reopened.claim(&id).is_err());
    assert!(reopened.cancel(&id).is_err());
}
#[test]
fn concurrent_operators_or_cancel_only_one_can_claim() {
    let dir = private_directory();
    let inbox = Inbox::open(dir.path()).unwrap();
    let id = inbox.enqueue(request()).unwrap().intent.id;
    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|_| {
            let inbox = inbox.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                inbox.claim(&id).is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .filter_map(|t| t.join().ok())
            .filter(|b| *b)
            .count(),
        1
    );
}
#[test]
fn cancellation_is_terminal_and_path_injection_is_rejected() {
    let dir = private_directory();
    let inbox = Inbox::open(dir.path()).unwrap();
    let id = inbox.enqueue(request()).unwrap().intent.id;
    assert_eq!(inbox.cancel(&id).unwrap().state, "cancelled");
    assert!(inbox.claim(&id).is_err());
    for id in ["../outside", "/tmp/something", ""] {
        assert!(inbox.status(id).is_err());
        assert!(inbox.claim(id).is_err());
    }
}
#[test]
fn truncated_journal_is_unknown_not_retryable() {
    let dir = private_directory();
    let inbox = Inbox::open(dir.path()).unwrap();
    let id = inbox.enqueue(request()).unwrap().intent.id;
    let mut claim = inbox.claim(&id).unwrap();
    claim
        .record("handoff_started_outcome_unknown", "test")
        .unwrap();
    drop(claim);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(format!("{id}.journal.jsonl")))
        .unwrap()
        .write_all(b"{partial")
        .unwrap();
    assert_eq!(inbox.status(&id).unwrap().state, "claimed_outcome_unknown");
    assert!(inbox.claim(&id).is_err());
}
#[test]
fn inbox_requires_private_directory_and_rejects_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = private_directory();
    let path = dir.path().join("inbox");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Inbox::open(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = dir.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(Inbox::open(&link).is_err());
    let inbox = Inbox::open(&path).unwrap();
    let id = inbox.enqueue(request()).unwrap().intent.id;
    let journal = path.join(format!("{id}.journal.jsonl"));
    symlink(dir.path().join("outside"), &journal).unwrap();
    assert!(inbox.claim(&id).is_err());
}

fn private_directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
