//! Append-only binary persistence for monitor history (completed calls and SDS).
//!
//! Format: a flat file of framed records. Each record is:
//!   [u32 little-endian length N][N bytes of bincode-serialized `StoredRecord`]
//!
//! Appends are durable in order and crash-safe on read: a torn trailing record
//! (partial length or body, e.g. from a crash mid-write) is detected and the
//! replay stops cleanly at the last complete record rather than erroring. The
//! log keeps everything (no rotation/compaction) by design.

use crate::monitor::{CallRecord, SdsRecord};
use crate::telemetry::SdsTelemetryRecord;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One persisted event, appended in the order it occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoredRecord {
    /// A completed call (recorded when it ends).
    Call(CallRecord),
    /// An SDS transfer.
    Sds(SdsRecord),
    /// A delivery report for a previously stored SDS (by uuid).
    SdsReport { uuid: uuid::Uuid },
    /// An SDS log entry observed on a FlowStation telemetry channel.
    SdsTelemetry(SdsTelemetryRecord),
}

/// Append-only log writer/reader. Cheap to clone-share via `Arc`.
pub struct Store {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl Store {
    /// Opens (creating if needed) the log at `path` for appending. Parent
    /// directories are created if missing.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        Ok(Self { path, file: Mutex::new(file) })
    }

    /// Appends one record, framed with a u32 length prefix, and flushes.
    pub fn append(&self, rec: &StoredRecord) -> std::io::Result<()> {
        let body = bincode::serialize(rec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = body.len() as u32;
        let mut f = self.file.lock().unwrap();
        f.write_all(&len.to_le_bytes())?;
        f.write_all(&body)?;
        f.flush()?;
        Ok(())
    }

    /// Reads the entire log from `path`, returning every complete record. A torn
    /// trailing record is ignored. A missing file yields an empty vector.
    pub fn replay(path: impl AsRef<Path>) -> std::io::Result<Vec<StoredRecord>> {
        let path = path.as_ref();
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 4 <= buf.len() {
            let len = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
            let start = pos + 4;
            let end = start + len;
            if end > buf.len() {
                // Torn trailing record — stop cleanly.
                break;
            }
            match bincode::deserialize::<StoredRecord>(&buf[start..end]) {
                Ok(rec) => out.push(rec),
                Err(_) => break, // corrupt frame; stop at last good record
            }
            pos = end;
        }
        Ok(out)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{CallRecord, SdsRecord};

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("brew-store-test-{}-{}.bin", name, uuid::Uuid::new_v4().simple()))
    }

    fn call(v: u64) -> CallRecord {
        CallRecord { uuid: uuid::Uuid::new_v4(), kind: "group".into(), source: 90, destination: 1001, priority: 0, started_at_ms: v, ended_at_ms: Some(v + 5), voice_frames: 3 }
    }
    fn sds(v: u64) -> SdsRecord {
        SdsRecord { uuid: uuid::Uuid::new_v4(), source: 90, destination: 100, at_ms: v, reports: 0 }
    }

    #[test]
    fn append_and_replay_roundtrip() {
        let p = tmp("roundtrip");
        let s = Store::open(&p).unwrap();
        s.append(&StoredRecord::Call(call(1))).unwrap();
        s.append(&StoredRecord::Sds(sds(2))).unwrap();
        let rep_uuid = uuid::Uuid::new_v4();
        s.append(&StoredRecord::SdsReport { uuid: rep_uuid }).unwrap();
        drop(s);

        let recs = Store::replay(&p).unwrap();
        assert_eq!(recs.len(), 3);
        assert!(matches!(recs[0], StoredRecord::Call(_)));
        assert!(matches!(recs[1], StoredRecord::Sds(_)));
        assert!(matches!(recs[2], StoredRecord::SdsReport { uuid } if uuid == rep_uuid));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn appends_persist_across_reopen() {
        let p = tmp("reopen");
        { let s = Store::open(&p).unwrap(); s.append(&StoredRecord::Call(call(1))).unwrap(); }
        { let s = Store::open(&p).unwrap(); s.append(&StoredRecord::Call(call(2))).unwrap(); }
        let recs = Store::replay(&p).unwrap();
        assert_eq!(recs.len(), 2, "append mode must not truncate");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn torn_trailing_record_is_ignored() {
        let p = tmp("torn");
        let s = Store::open(&p).unwrap();
        s.append(&StoredRecord::Call(call(1))).unwrap();
        drop(s);
        // Corrupt: append a bogus length prefix with no/short body.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&9999u32.to_le_bytes()).unwrap();
        f.write_all(&[1, 2, 3]).unwrap(); // far short of 9999
        drop(f);

        let recs = Store::replay(&p).unwrap();
        assert_eq!(recs.len(), 1, "should recover the one complete record and stop");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn missing_file_replays_empty() {
        let p = tmp("missing");
        let recs = Store::replay(&p).unwrap();
        assert!(recs.is_empty());
    }
}

