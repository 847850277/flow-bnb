//! Durable intent records shared by native execution and legacy signer handoff.
//! A claim deduplicates a request; a saved file alone never starts execution.
use crate::trade::TradeRequest;
use anyhow::{ensure, Context, Result};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct TypedInbox<T> {
    directory: PathBuf,
    request_type: PhantomData<T>,
}
pub type Inbox = TypedInbox<TradeRequest>;
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Intent<T = TradeRequest> {
    pub schema_version: u32,
    pub id: String,
    pub created_at: String,
    pub request: T,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    pub id: String,
    pub state: String,
    pub updated_at: String,
    pub message: String,
    #[serde(default)]
    pub result: Option<ExecutionSummary>,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionSummary {
    pub evidence_kind: String,
    pub tx_hash: Option<String>,
    pub receipt_success: Option<bool>,
    pub sell_delta: Option<String>,
    pub buy_delta: Option<String>,
}
#[derive(Serialize, JsonSchema)]
pub struct Queued<T = TradeRequest> {
    pub intent: Intent<T>,
    pub status: Status,
    pub next_action: String,
}
impl<T: Clone + Serialize + DeserializeOwned> TypedInbox<T> {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref();
        ensure!(
            directory.is_absolute(),
            "handoff directory must be absolute"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            if !directory.exists() {
                fs::DirBuilder::new().mode(0o700).create(directory)?;
            }
            let meta = fs::symlink_metadata(directory)?;
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "handoff directory must be a real directory"
            );
            ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "handoff directory must have mode 0700"
            );
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("operator handoff currently requires Unix");
        }
        Ok(Self {
            directory: directory.canonicalize()?,
            request_type: PhantomData,
        })
    }
    fn path(&self, id: &str, suffix: &str) -> Result<PathBuf> {
        ensure!(
            id.len() == 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid intent ID"
        );
        Ok(self.directory.join(format!("{id}.{suffix}")))
    }
    pub fn enqueue(&self, request: T) -> Result<Queued<T>> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let entropy = format!(
            "{}:{}:{}:{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            serde_json::to_string(&request)?
        );
        let id = format!("{:x}", Sha256::digest(entropy.as_bytes()));
        self.enqueue_with_id(request, id)
    }
    /// Stable IDs let callers retry queue creation, never execution.
    pub(crate) fn enqueue_with_id(&self, request: T, id: String) -> Result<Queued<T>> {
        ensure!(
            serde_json::to_vec(&request)?.len() < 8192,
            "request too large"
        );
        let path = self.path(&id, "intent.json")?;
        if path.try_exists()? {
            let intent = self.intent(&id)?;
            ensure!(
                serde_json::to_value(&intent.request)? == serde_json::to_value(&request)?,
                "request ID already belongs to a different intent or configuration"
            );
            return Ok(Queued {
                intent,
                status: self.status(&id)?,
                next_action:
                    "Existing intent returned; never enqueue a replacement to retry execution."
                        .into(),
            });
        }
        // Do not let a client fill the operator's disk indefinitely.
        ensure!(
            fs::read_dir(&self.directory)?.take(4097).count() < 4096,
            "handoff inbox full; operator must archive records"
        );
        let intent = Intent {
            schema_version: 1,
            id: id.clone(),
            created_at: now(),
            request,
        };
        let mut file = create(&self.path(&id, "intent.json")?)?;
        serde_json::to_writer(&mut file, &intent)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        File::open(&self.directory)?.sync_all()?;
        let status = Status {
            id: id.clone(),
            state: "awaiting_operator".into(),
            updated_at: now(),
            message: "Intent queued; no signature or transaction has been requested.".into(),
            result: None,
        };
        Ok(Queued {intent,status,next_action:"An operator must run flow-bnb approve-trade with this ID and operator-owned policy/wallet configuration. The CLI prepares a fresh quote and submits through the selected wallet without a terminal prompt.".into()})
    }
    pub fn intent(&self, id: &str) -> Result<Intent<T>> {
        let path = self.path(id, "intent.json")?;
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 8192,
            "invalid intent file"
        );
        let intent: Intent<T> = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            intent.id == id && intent.schema_version == 1,
            "intent identity mismatch"
        );
        Ok(intent)
    }
    pub fn status(&self, id: &str) -> Result<Status> {
        let mut status = self.journal_status(id)?;
        let path = self.path(id, "audit.json")?;
        // Audit data is diagnostic, never used as authority to replay or approve.
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 1_048_576 {
                if let Ok(value) = fs::read(path).and_then(|b| {
                    serde_json::from_slice::<serde_json::Value>(&b).map_err(std::io::Error::other)
                }) {
                    let hash = value["tx_hash"]
                        .as_str()
                        .filter(|s| {
                            s.len() == 66
                                && s.starts_with("0x")
                                && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
                        })
                        .map(str::to_owned);
                    let delta = |field: &str| {
                        value["settlement"][field]
                            .as_str()
                            .filter(|s| {
                                s.len() <= 79
                                    && !s.is_empty()
                                    && s.trim_start_matches('-')
                                        .bytes()
                                        .all(|b| b.is_ascii_digit())
                            })
                            .map(str::to_owned)
                    };
                    status.result = Some(ExecutionSummary {
                        evidence_kind: if value["mode"] == "simulation" {
                            "simulation"
                        } else {
                            "local_development_node"
                        }
                        .into(),
                        tx_hash: hash,
                        receipt_success: value["settlement"]["receipt"]["success"].as_bool(),
                        sell_delta: delta("sell_delta"),
                        buy_delta: delta("buy_delta"),
                    });
                }
            }
        }
        Ok(status)
    }
    fn journal_status(&self, id: &str) -> Result<Status> {
        let intent = self.intent(id)?;
        let path = self.path(id, "journal.jsonl")?;
        if fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            return Ok(Status {
                id: id.into(),
                state: "awaiting_operator".into(),
                updated_at: intent.created_at,
                message: "Awaiting operator; no transaction submitted.".into(),
                result: None,
            });
        }
        // An interrupted/truncated journal is never interpreted as retryable.
        let mut last = unknown(id);
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 65536,
            "invalid journal file"
        );
        for line in BufReader::new(File::open(path)?).lines() {
            let Ok(line) = line else {
                return Ok(unknown(id));
            };
            match serde_json::from_str::<Status>(&line) {
                Ok(status) if status.id == id => last = status,
                _ => return Ok(unknown(id)),
            }
        }
        Ok(last)
    }
    pub fn pending(&self) -> Result<Vec<Intent<T>>> {
        let mut pending = vec![];
        for entry in fs::read_dir(&self.directory)?.take(4096) {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name.strip_suffix(".intent.json") else {
                continue;
            };
            if self.journal_status(id)?.state == "awaiting_operator" {
                pending.push(self.intent(id)?);
            }
        }
        pending.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(pending)
    }
    pub fn claim(&self, id: &str) -> Result<Claim<T>> {
        let intent = self.intent(id)?;
        // create_new is the inter-process lock and permanent replay barrier.
        let file = create(&self.path(id, "journal.jsonl")?)
            .context("intent already claimed or cancelled; inspect status instead of retrying")?;
        file.try_lock().context("claim journal is in use")?;
        File::open(&self.directory)?.sync_all()?;
        let mut claim = Claim {
            intent,
            file,
            audit_path: self.path(id, "audit.json")?,
        };
        claim.record("preparing", "Execution claimed; preparing a fresh quote.")?;
        Ok(claim)
    }
    pub fn cancel(&self, id: &str) -> Result<Status> {
        let mut claim = self.claim(id)?;
        claim.record("cancelled", "Cancelled before operator execution.")?;
        self.status(id)
    }
}
pub struct Claim<T = TradeRequest> {
    pub intent: Intent<T>,
    pub audit_path: PathBuf,
    file: File,
}
impl<T> Claim<T> {
    pub fn record(&mut self, state: &str, message: &str) -> Result<()> {
        let status = Status {
            id: self.intent.id.clone(),
            state: state.into(),
            updated_at: now(),
            message: message.into(),
            result: None,
        };
        let mut bytes = serde_json::to_vec(&status)?;
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        Ok(())
    }
}
fn create(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn unknown(id: &str) -> Status {
    Status {
        id: id.into(),
        state: "claimed_outcome_unknown".into(),
        updated_at: now(),
        message: "Journal incomplete; inspect audit and wallet before any new intent.".into(),
        result: None,
    }
}
