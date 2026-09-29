//! Sidecar journal in the thread history directory. A decision is not exposed
//! to an external effect handler until its record has been synced to storage.

use codex_app_server_protocol::InputMiddlewareRecord;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const MAX_JOURNAL_BYTES: u64 = 160 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;

pub(crate) fn path_for_rollout(rollout: &Path, thread_id: ThreadId) -> io::Result<PathBuf> {
    // Rollouts live in HISTORY/sessions/YYYY/MM/DD/rollout-*.jsonl. Keep the
    // sidecar outside sessions so the rollout scanner never mistakes it for a turn.
    let sessions = rollout
        .ancestors()
        .nth(4)
        .ok_or_else(|| io::Error::other("rollout has no sessions directory"))?;
    if sessions.file_name() != Some(std::ffi::OsStr::new("sessions")) {
        return Err(io::Error::other(
            "rollout is not in a standard sessions directory",
        ));
    }
    let root = rollout
        .ancestors()
        .nth(5)
        .ok_or_else(|| io::Error::other("rollout has no history root"))?;
    Ok(root
        .join("input-middleware")
        .join(format!("{thread_id}.jsonl")))
}

pub(crate) async fn load(path: PathBuf) -> io::Result<HashMap<String, InputMiddlewareRecord>> {
    tokio::task::spawn_blocking(move || {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error),
        };
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(io::Error::other("input middleware journal too large"));
        }
        let mut records = HashMap::<String, InputMiddlewareRecord>::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let record: InputMiddlewareRecord = serde_json::from_slice(line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if !records.contains_key(&record.input_id) && records.len() == MAX_RECORDS {
                return Err(io::Error::other("input middleware journal is full"));
            }
            records.insert(record.input_id.clone(), record);
        }
        Ok(records)
    })
    .await
    .map_err(io::Error::other)?
}

pub(crate) async fn append(path: PathBuf, record: &InputMiddlewareRecord) -> io::Result<()> {
    let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&line)?;
        file.sync_all()
    })
    .await
    .map_err(io::Error::other)?
}
