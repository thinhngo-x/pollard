//! Remotes for pollard (§5): a local path or an S3-compatible URL, via `object_store`.
//!
//! Layout (mirrors `.pollard/`): `objects/ab/cdef…` and `chunkmaps/ab/cdef…` as individual
//! files in stored form, chunks in `packs/<name>.pack` (concatenated zstd chunks, ≤ 64 MB)
//! with `packs/<name>.idx` (`hash offset len` lines, written after the pack),
//! plus `nodes.jsonl` and `pins.json` (owned by pollard-core's sync).
//! Blobs are content-addressed, so a push only uploads what the remote lacks.

use std::collections::HashSet;
use std::sync::Arc;

use futures::TryStreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as OPath;
use object_store::prefix::PrefixStore;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pollard_objects::Store;

const PACK_LIMIT: usize = 64 << 20;

/// `(chunk hash, offset, len)` entries of one pack.
type PackIndex = Vec<(String, u64, u64)>;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("remote {url}: {source}")]
    Store { url: String, source: object_store::Error },
    #[error("remote {url}: bad pack index {name}")]
    BadIndex { url: String, name: String },
    #[error(transparent)]
    Objects(#[from] pollard_objects::Error),
    #[error("remote {0}: {1}")]
    Setup(String, String),
}

/// Bytes and files uploaded or downloaded by one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
}

impl Stats {
    fn add(&mut self, n: usize) {
        self.files += 1;
        self.bytes += n as u64;
    }
}

pub struct Remote {
    url: String,
    store: Arc<dyn ObjectStore>,
    rt: tokio::runtime::Runtime,
}

impl Remote {
    /// `s3://bucket/prefix` (credentials/endpoint from the usual `AWS_*` env vars) or a local
    /// directory path (created if missing).
    pub fn open(url: &str) -> Result<Remote> {
        let setup = |e: &dyn std::fmt::Display| Error::Setup(url.into(), e.to_string());
        let store: Arc<dyn ObjectStore> = if let Some(rest) = url.strip_prefix("s3://") {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            let s3 = AmazonS3Builder::from_env().with_bucket_name(bucket).build().map_err(|e| setup(&e))?;
            if prefix.trim_matches('/').is_empty() {
                Arc::new(s3)
            } else {
                Arc::new(PrefixStore::new(s3, prefix.trim_matches('/')))
            }
        } else if url.contains("://") {
            return Err(setup(&"only s3:// URLs and local paths are supported"));
        } else {
            std::fs::create_dir_all(url).map_err(|e| setup(&e))?;
            Arc::new(LocalFileSystem::new_with_prefix(url).map_err(|e| setup(&e))?)
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| setup(&e))?;
        Ok(Remote { url: url.into(), store, rt })
    }

    fn err(&self, source: object_store::Error) -> Error {
        Error::Store { url: self.url.clone(), source }
    }

    /// Whole file, or `None` if absent.
    pub fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let r = self.rt.block_on(async {
            match self.store.get(&OPath::from(name)).await {
                Ok(g) => g.bytes().await.map(Some),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(e),
            }
        });
        r.map(|b| b.map(|b| b.to_vec())).map_err(|e| self.err(e))
    }

    pub fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        self.rt
            .block_on(self.store.put(&OPath::from(name), PutPayload::from(bytes)))
            .map(|_| ())
            .map_err(|e| self.err(e))
    }

    /// Names (relative to the remote root) under `prefix`.
    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let v: Vec<_> = self
            .rt
            .block_on(self.store.list(Some(&OPath::from(prefix))).try_collect::<Vec<_>>())
            .map_err(|e| self.err(e))?;
        Ok(v.into_iter().map(|m| m.location.to_string()).collect())
    }

    /// Hashes stored individually under `kind/ab/cdef…`.
    fn hashes(&self, kind: &str) -> Result<HashSet<String>> {
        Ok(self.list(kind)?.iter().filter_map(|n| n.strip_prefix(&format!("{kind}/"))).map(|h| h.replace('/', "")).collect())
    }

    /// `(pack name, [(hash, offset, len)])` for every pack index.
    fn packs(&self) -> Result<Vec<(String, PackIndex)>> {
        let mut out = Vec::new();
        for name in self.list("packs")?.into_iter().filter(|n| n.ends_with(".idx")) {
            let text = self.get(&name)?.unwrap_or_default();
            let bad = || Error::BadIndex { url: self.url.clone(), name: name.clone() };
            let mut entries = Vec::new();
            for line in String::from_utf8(text).map_err(|_| bad())?.lines() {
                let mut it = line.split(' ');
                let (Some(h), Some(o), Some(l)) = (it.next(), it.next(), it.next()) else { return Err(bad()) };
                entries.push((h.to_string(), o.parse().map_err(|_| bad())?, l.parse().map_err(|_| bad())?));
            }
            out.push((name.trim_end_matches(".idx").to_string(), entries));
        }
        Ok(out)
    }

    /// Upload every local blob the remote lacks: chunks (packed), then chunk maps, then
    /// objects, so the remote never references data it does not have.
    // ponytail: reads every pack index on each push/pull; cache them locally if packs pile up.
    pub fn push_blobs(&self, store: &Store) -> Result<Stats> {
        let mut st = Stats::default();
        let remote_chunks: HashSet<String> = self.packs()?.into_iter().flat_map(|p| p.1).map(|e| e.0).collect();
        let mut pack = Vec::new();
        let mut idx = String::new();
        for (hash, _, _) in store.list("chunks")? {
            if remote_chunks.contains(&hash) {
                continue;
            }
            let z = store.read_raw("chunks", &hash)?;
            idx.push_str(&format!("{hash} {} {}\n", pack.len(), z.len()));
            pack.extend_from_slice(&z);
            if pack.len() >= PACK_LIMIT {
                self.put_pack(&mut pack, &mut idx, &mut st)?;
            }
        }
        self.put_pack(&mut pack, &mut idx, &mut st)?;
        for kind in ["chunkmaps", "objects"] {
            let have = self.hashes(kind)?;
            for (hash, _, _) in store.list(kind)? {
                if !have.contains(&hash) {
                    let bytes = store.read_raw(kind, &hash)?;
                    st.add(bytes.len());
                    self.put(&format!("{kind}/{}/{}", &hash[..2], &hash[2..]), bytes)?;
                }
            }
        }
        Ok(st)
    }

    fn put_pack(&self, pack: &mut Vec<u8>, idx: &mut String, st: &mut Stats) -> Result<()> {
        if pack.is_empty() {
            return Ok(());
        }
        let name = format!("packs/{}", pollard_objects::hash_bytes(idx.as_bytes()));
        st.add(pack.len());
        st.add(idx.len());
        self.put(&format!("{name}.pack"), std::mem::take(pack))?;
        self.put(&format!("{name}.idx"), std::mem::take(idx).into_bytes())
    }

    /// Download every remote blob missing locally (chunks by byte range from their packs).
    pub fn pull_blobs(&self, store: &Store) -> Result<Stats> {
        let mut st = Stats::default();
        for (name, entries) in self.packs()? {
            let missing: Vec<_> = entries.into_iter().filter(|(h, _, _)| !store.has_raw("chunks", h)).collect();
            if missing.is_empty() {
                continue;
            }
            let ranges: Vec<_> = missing.iter().map(|(_, o, l)| *o..o + l).collect();
            let path = OPath::from(format!("{name}.pack"));
            let got = self.rt.block_on(self.store.get_ranges(&path, &ranges)).map_err(|e| self.err(e))?;
            for ((hash, _, _), bytes) in missing.iter().zip(got) {
                st.add(bytes.len());
                store.write_raw("chunks", hash, &bytes)?;
            }
        }
        for kind in ["chunkmaps", "objects"] {
            for hash in self.hashes(kind)? {
                if store.has_raw(kind, &hash) {
                    continue;
                }
                if let Some(bytes) = self.get(&format!("{kind}/{}/{}", &hash[..2.min(hash.len())], &hash[2.min(hash.len())..]))? {
                    st.add(bytes.len());
                    store.write_raw(kind, &hash, &bytes)?;
                }
            }
        }
        Ok(st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn push_pull_roundtrip_and_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let a = Store::new(t.path().join("a"));
        let b = Store::new(t.path().join("b"));
        let big = noise(3 << 20, 5);
        let (hbig, _) = a.put_bytes(&big).unwrap();
        let hsmall = a.put_object(b"small").unwrap();
        let remote = Remote::open(t.path().join("remote").to_str().unwrap()).unwrap();

        let first = remote.push_blobs(&a).unwrap();
        assert!(first.bytes > 2 << 20, "{first:?}");
        assert_eq!(remote.push_blobs(&a).unwrap(), Stats::default(), "second push uploads nothing");
        assert!(t.path().join("remote/objects").is_dir());

        let pulled = remote.pull_blobs(&b).unwrap();
        assert!(pulled.bytes > 2 << 20);
        assert_eq!(b.read_blob(&hbig).unwrap(), big);
        assert_eq!(b.get_object(&hsmall).unwrap(), b"small");
        assert_eq!(remote.pull_blobs(&b).unwrap(), Stats::default());

        // an edited copy only uploads its new chunks
        let mut big2 = big.clone();
        big2[1 << 20..(1 << 20) + 1000].fill(0);
        b.put_bytes(&big2).unwrap();
        let second = remote.push_blobs(&b).unwrap();
        assert!(second.bytes < 1 << 20, "{second:?}");

        assert_eq!(remote.get("nodes.jsonl").unwrap(), None);
        remote.put("nodes.jsonl", b"x\n".to_vec()).unwrap();
        assert_eq!(remote.get("nodes.jsonl").unwrap().unwrap(), b"x\n");
        assert!(Remote::open("ftp://x").is_err());
    }
}
