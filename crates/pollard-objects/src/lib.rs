//! Content-addressed storage for pollard: Tier 1 objects (small blobs, manifests)
//! and Tier 2 chunks (CDC-chunked large files). All hashes are blake3 hex strings.

mod manifest;
mod store;

use std::path::{Path, PathBuf};

pub use manifest::{
    Change, ChangeKind, Entry, MODE_EXEC, MODE_FILE, MODE_LINK, Manifest, Snapshot, WalkOptions, diff, scan_dir,
};
pub use store::{GcStats, SMALL_LIMIT, Store};

/// Files larger than this are left out of code manifests (§3) and reported as oversized.
pub const CODE_FILE_LIMIT: u64 = 10 * 1024 * 1024;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("object {0} not found")]
    NotFound(String),
    #[error("object {0} is corrupt (hash mismatch)")]
    Corrupt(String),
    #[error("manifest line {line}: {reason}")]
    BadManifest { line: usize, reason: String },
    #[error("unsupported path {0:?} (must be UTF-8 without newlines)")]
    BadPath(PathBuf),
    #[error("walking directory: {0}")]
    Walk(#[from] ignore::Error),
}

pub(crate) trait IoCtx<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoCtx<T> for std::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| Error::Io { path: path.to_path_buf(), source })
    }
}

/// blake3 hex of a byte slice.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// blake3 hex of a file's contents, streamed.
pub fn hash_file(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).at(path)?;
    let mut h = blake3::Hasher::new();
    h.update_reader(f).at(path)?;
    Ok(h.finalize().to_hex().to_string())
}
