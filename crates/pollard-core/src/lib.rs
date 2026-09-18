//! pollard-core: node model, SQLite store, op log, deltas, siblings, metrics, run protocol.

pub mod cmd;
pub mod config;
pub mod delta;
pub mod env;
pub mod gitops;
pub mod ids;
pub mod metrics;
pub mod node;
pub mod ops;
pub mod repo;
pub mod run;
pub mod siblings;
pub mod sync;
pub mod tree;
pub mod wc;
pub mod weights;

pub use node::{Node, Status};
pub use ops::OpRecord;
pub use repo::Repo;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a pollard repo (no .pollard/ in {0} or any parent)")]
    NotARepo(PathBuf),
    #[error("already a pollard repo: {0}")]
    AlreadyInit(PathBuf),
    #[error("unknown node: {0}")]
    UnknownNode(String),
    #[error("ambiguous node prefix '{0}': matches {1}")]
    Ambiguous(String, String),
    #[error("duplicate recipe: same code/config/data/env as node {0} (use --force)")]
    Duplicate(String),
    #[error("{0}")]
    Msg(String),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("objects: {0}")]
    Objects(#[from] pollard_objects::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn msg(s: impl Into<String>) -> Error {
    Error::Msg(s.into())
}

/// Attach a path to an io::Error.
pub(crate) trait IoCtx<T> {
    fn at(self, p: impl Into<PathBuf>) -> Result<T>;
}
impl<T> IoCtx<T> for std::io::Result<T> {
    fn at(self, p: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::Io(p.into(), e))
    }
}

/// UTC timestamp `YYYY-MM-DDTHH:MM:SS.ffffffZ` (lexicographically sortable).
pub fn now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_micros()
    )
}

pub(crate) fn b3(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
