use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fastcdc::v2020::StreamCDC;

use crate::manifest::{self, MODE_EXEC, MODE_LINK};
use crate::{Entry, Error, IoCtx, Manifest, Result, Snapshot, WalkOptions, hash_bytes};

/// Blobs under this size are stored whole in `objects/`; larger ones are CDC-chunked.
pub const SMALL_LIMIT: u64 = 1 << 20;
const CHUNK_MIN: usize = 8 * 1024;
const CHUNK_AVG: usize = 64 * 1024;
const CHUNK_MAX: usize = 128 * 1024;
const ZSTD_LEVEL: i32 = 3;

/// On-disk store rooted at `.pollard/`:
/// `objects/ab/cdef…` (zstd blobs), `chunks/ab/cdef…` (zstd CDC chunks, keyed by blake3 of
/// the raw chunk), `chunkmaps/ab/cdef…` (text, one `chunk_hash size` line per chunk, keyed by
/// blake3 of the whole file). A blob hash resolves to either an object or a chunk map.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcStats {
    pub chunks_removed: usize,
    pub maps_removed: usize,
    /// Compressed bytes freed on disk.
    pub bytes_freed: u64,
}

impl Store {
    /// `root` is the `.pollard/` directory; `objects/`, `chunks/`, `chunkmaps/` are created on demand.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn shard(&self, kind: &str, hash: &str) -> PathBuf {
        let (a, b) = hash.split_at(2.min(hash.len()));
        self.root.join(kind).join(a).join(b)
    }

    fn write_atomic(&self, dest: &Path, bytes: &[u8]) -> Result<()> {
        if dest.exists() {
            return Ok(());
        }
        let dir = dest.parent().expect("sharded path has a parent");
        fs::create_dir_all(dir).at(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).at(dir)?;
        tmp.write_all(bytes).at(dest)?;
        tmp.persist(dest).map_err(|e| e.error).at(dest)?;
        Ok(())
    }

    fn put_raw(&self, kind: &str, bytes: &[u8]) -> Result<String> {
        let hash = hash_bytes(bytes);
        let dest = self.shard(kind, &hash);
        if !dest.exists() {
            let z = zstd::encode_all(bytes, ZSTD_LEVEL).at(&dest)?;
            self.write_atomic(&dest, &z)?;
        }
        Ok(hash)
    }

    fn get_raw(&self, kind: &str, hash: &str) -> Result<Vec<u8>> {
        let path = self.shard(kind, hash);
        let z = match fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(hash.into()));
            }
            r => r.at(&path)?,
        };
        let bytes = zstd::decode_all(&z[..]).map_err(|_| Error::Corrupt(hash.into()))?;
        if hash_bytes(&bytes) != hash {
            return Err(Error::Corrupt(hash.into()));
        }
        Ok(bytes)
    }

    /// Store a small blob whole (zstd 3). Returns its blake3 hex.
    pub fn put_object(&self, bytes: &[u8]) -> Result<String> {
        self.put_raw("objects", bytes)
    }

    /// Read and verify a whole object. `NotFound` if absent (chunked blobs are not objects;
    /// use `write_blob`/`read_blob` for those).
    pub fn get_object(&self, hash: &str) -> Result<Vec<u8>> {
        self.get_raw("objects", hash)
    }

    pub fn has_object(&self, hash: &str) -> bool {
        self.shard("objects", hash).exists()
    }

    /// True if `hash` is stored either as an object or as a chunk map.
    pub fn has_blob(&self, hash: &str) -> bool {
        self.has_object(hash) || self.shard("chunkmaps", hash).exists()
    }

    pub fn put_manifest(&self, m: &Manifest) -> Result<String> {
        self.put_object(&m.to_bytes())
    }

    pub fn get_manifest(&self, hash: &str) -> Result<Manifest> {
        Manifest::parse(&self.get_object(hash)?)
    }

    /// Store a file's contents: under `SMALL_LIMIT` as one object, otherwise CDC-chunked
    /// (streamed, never loaded whole). Returns `(blake3 of contents, size)`.
    pub fn put_file(&self, path: &Path) -> Result<(String, u64)> {
        let f = File::open(path).at(path)?;
        let len = f.metadata().at(path)?.len();
        if len < SMALL_LIMIT {
            let bytes = fs::read(path).at(path)?;
            return Ok((self.put_object(&bytes)?, bytes.len() as u64));
        }
        self.put_chunked(BufReader::new(f), path)
    }

    /// Same as `put_file` for bytes already in memory.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<(String, u64)> {
        if (bytes.len() as u64) < SMALL_LIMIT {
            return Ok((self.put_object(bytes)?, bytes.len() as u64));
        }
        self.put_chunked(bytes, Path::new("<memory>"))
    }

    fn put_chunked(&self, r: impl std::io::Read, what: &Path) -> Result<(String, u64)> {
        let mut hasher = blake3::Hasher::new();
        let mut map = String::new();
        let mut size = 0u64;
        for c in StreamCDC::new(r, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
            let c = c.map_err(std::io::Error::from).at(what)?;
            hasher.update(&c.data);
            let h = self.put_raw("chunks", &c.data)?;
            map.push_str(&format!("{h} {}\n", c.length));
            size += c.length as u64;
        }
        let hash = hasher.finalize().to_hex().to_string();
        self.write_atomic(&self.shard("chunkmaps", &hash), map.as_bytes())?;
        Ok((hash, size))
    }

    /// Chunk list `(chunk_hash, raw_size)` of a chunked blob, or `None` if it is not chunked.
    pub fn chunks_of(&self, hash: &str) -> Result<Option<Vec<(String, u64)>>> {
        let path = self.shard("chunkmaps", hash);
        let text = match fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            r => r.at(&path)?,
        };
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let parsed = line
                .split_once(' ')
                .and_then(|(h, n)| Some((h.to_string(), n.parse().ok()?)));
            out.push(parsed.ok_or(Error::BadManifest {
                line: i + 1,
                reason: format!("bad chunk map {hash}"),
            })?);
        }
        Ok(Some(out))
    }

    /// Stream a blob (object or chunked) into `w`, verifying its hash.
    pub fn write_blob(&self, hash: &str, w: &mut dyn Write) -> Result<()> {
        let io = |e| Error::Io {
            path: PathBuf::from(format!("<blob {hash}>")),
            source: e,
        };
        if self.has_object(hash) {
            return w.write_all(&self.get_object(hash)?).map_err(io);
        }
        let chunks = self
            .chunks_of(hash)?
            .ok_or_else(|| Error::NotFound(hash.into()))?;
        let mut hasher = blake3::Hasher::new();
        for (c, _) in chunks {
            let data = self.get_raw("chunks", &c)?;
            hasher.update(&data);
            w.write_all(&data).map_err(io)?;
        }
        if hasher.finalize().to_hex().as_str() != hash {
            return Err(Error::Corrupt(hash.into()));
        }
        Ok(())
    }

    pub fn read_blob(&self, hash: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write_blob(hash, &mut out)?;
        Ok(out)
    }

    /// Check every destination before any working-copy mutation. Symlink entries are
    /// allowed, but neither existing nor manifest-provided symlinks may be parents.
    pub fn validate_destinations(&self, m: &Manifest, root: &Path) -> Result<()> {
        self.validate_with_removals(m, root, &HashSet::new())
    }

    fn validate_with_removals(
        &self,
        m: &Manifest,
        root: &Path,
        removed: &HashSet<&str>,
    ) -> Result<()> {
        let paths: HashSet<&str> = m.entries.iter().map(|e| e.path.as_str()).collect();
        for e in &m.entries {
            let path = Path::new(&e.path);
            if e.path.is_empty()
                || e.path
                    .split('/')
                    .any(|p| matches!(p, "" | "." | ".." | ".git" | ".pollard"))
                || path.is_absolute()
                || e.path.contains(['\0', '\n'])
            {
                return Err(Error::BadPath(path.into()));
            }
            let parents: Vec<_> = path
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            if parents
                .iter()
                .any(|parent| paths.contains(parent.to_str().unwrap()))
            {
                return Err(Error::BadPath(path.into()));
            }
            for parent in parents.into_iter().rev() {
                if removed.contains(parent.to_str().unwrap()) {
                    break;
                }
                let dest = root.join(parent);
                match fs::symlink_metadata(&dest) {
                    Ok(md) if md.file_type().is_symlink() => return Err(Error::BadPath(dest)),
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(Error::Io {
                            path: dest,
                            source: e,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Restore a repository-relative entry after checking its destination.
    pub fn restore_at(&self, e: &Entry, root: &Path) -> Result<()> {
        self.validate_destinations(&Manifest::new(vec![e.clone()]), root)?;
        self.restore(e, &fs::canonicalize(root).at(root)?.join(&e.path))
    }

    /// Write one manifest entry's content to `dest` (atomically, with its mode;
    /// symlinks recreated). Parent directories are created.
    pub fn restore(&self, e: &Entry, dest: &Path) -> Result<()> {
        let dir = dest.parent().expect("dest has a parent");
        for parent in dir.ancestors() {
            if fs::symlink_metadata(parent).is_ok_and(|md| md.file_type().is_symlink()) {
                return Err(Error::BadPath(parent.into()));
            }
        }
        fs::create_dir_all(dir).at(dir)?;
        if e.mode == MODE_LINK {
            let target = self.read_blob(&e.hash)?;
            if fs::symlink_metadata(dest).is_ok() {
                fs::remove_file(dest).at(dest)?;
            }
            let target = std::ffi::OsStr::from_bytes(&target);
            return std::os::unix::fs::symlink(target, dest).at(dest);
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir).at(dir)?;
        self.write_blob(&e.hash, tmp.as_file_mut())?;
        let perm = if e.mode == MODE_EXEC { 0o755 } else { 0o644 };
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(perm))
            .at(dest)?;
        tmp.persist(dest).map_err(|e| e.error).at(dest)?;
        Ok(())
    }

    /// Store every file under `root` that passes `opts`, plus the manifest object itself.
    /// The manifest's object id is `snapshot.manifest.hash()`.
    pub fn snapshot_dir(&self, root: &Path, opts: &WalkOptions) -> Result<Snapshot> {
        let snap = manifest::build(root, opts, |abs, md| {
            if md.file_type().is_symlink() {
                let t = fs::read_link(abs).at(abs)?;
                let t = t.as_os_str().as_bytes();
                Ok((self.put_object(t)?, t.len() as u64))
            } else {
                self.put_file(abs)
            }
        })?;
        self.put_manifest(&snap.manifest)?;
        Ok(snap)
    }

    /// Make the files under `root` selected by `opts` match `m` exactly: extra files are
    /// deleted (and emptied directories removed), missing or different ones written.
    /// Files outside `opts` (ignored, off-tree, `.pollard/`) are never touched.
    /// Checks every blob exists before changing anything.
    pub fn materialize(&self, m: &Manifest, root: &Path, opts: &WalkOptions) -> Result<()> {
        let current = crate::scan_dir(root, opts)?.manifest;
        let removed: HashSet<&str> = current
            .entries
            .iter()
            .filter(|e| m.get(&e.path).is_none())
            .map(|e| e.path.as_str())
            .collect();
        self.validate_with_removals(m, root, &removed)?;
        if let Some(e) = m.entries.iter().find(|e| !self.has_blob(&e.hash)) {
            return Err(Error::NotFound(format!("{} (for {})", e.hash, e.path)));
        }
        for e in &current.entries {
            if m.get(&e.path).is_some() {
                continue;
            }
            let p = root.join(&e.path);
            fs::remove_file(&p).at(&p)?;
            let mut dir = p.parent();
            while let Some(d) = dir.filter(|d| *d != root) {
                if fs::remove_dir(d).is_err() {
                    break; // not empty
                }
                dir = d.parent();
            }
        }
        for e in &m.entries {
            if current
                .get(&e.path)
                .is_some_and(|c| c.hash == e.hash && c.mode == e.mode)
            {
                continue;
            }
            self.restore_at(e, root)?;
        }
        Ok(())
    }

    /// Mark-and-sweep over chunked blobs: keeps chunk maps and chunks reachable from the
    /// given manifests, deletes the rest. Objects are never deleted. Fails (deleting nothing)
    /// if any live manifest is missing. Run under the repo lock.
    pub fn gc(&self, live_manifests: &[String]) -> Result<GcStats> {
        let mut maps = HashSet::new();
        let mut chunks = HashSet::new();
        for mh in live_manifests {
            for e in self.get_manifest(mh)?.entries {
                if let Some(list) = self.chunks_of(&e.hash)? {
                    chunks.extend(list.into_iter().map(|(c, _)| c));
                    maps.insert(e.hash);
                }
            }
        }
        let mut stats = GcStats::default();
        for (hash, path, size) in self.list("chunkmaps")? {
            if !maps.contains(&hash) {
                fs::remove_file(&path).at(&path)?;
                stats.maps_removed += 1;
                stats.bytes_freed += size;
            }
        }
        for (hash, path, size) in self.list("chunks")? {
            if !chunks.contains(&hash) {
                fs::remove_file(&path).at(&path)?;
                stats.chunks_removed += 1;
                stats.bytes_freed += size;
            }
        }
        Ok(stats)
    }

    /// Bytes of one entry exactly as stored (`objects`/`chunks`: zstd, `chunkmaps`: text).
    /// For remotes, which mirror the on-disk form.
    pub fn read_raw(&self, kind: &str, hash: &str) -> Result<Vec<u8>> {
        let p = self.shard(kind, hash);
        fs::read(&p).at(&p)
    }

    pub fn has_raw(&self, kind: &str, hash: &str) -> bool {
        self.shard(kind, hash).exists()
    }

    /// Insert an entry received in stored form. Rejects non-hash names; objects and chunks are
    /// verified (decompress + blake3) before they land. Chunk maps are verified on read.
    pub fn write_raw(&self, kind: &str, hash: &str, bytes: &[u8]) -> Result<()> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Corrupt(hash.into()));
        }
        if kind != "chunkmaps" {
            let raw = zstd::decode_all(bytes).map_err(|_| Error::Corrupt(hash.into()))?;
            if hash_bytes(&raw) != hash {
                return Err(Error::Corrupt(hash.into()));
            }
        }
        self.write_atomic(&self.shard(kind, hash), bytes)
    }

    /// All stored entries of one kind (`objects`, `chunks`, `chunkmaps`): `(hash, path, disk size)`.
    /// Skips temp files. Used by gc and by remotes to enumerate what to transfer.
    pub fn list(&self, kind: &str) -> Result<Vec<(String, PathBuf, u64)>> {
        let base = self.root.join(kind);
        let mut out = Vec::new();
        let Ok(shards) = fs::read_dir(&base) else {
            return Ok(out);
        };
        for shard in shards {
            let shard = shard.at(&base)?;
            let prefix = shard.file_name().to_string_lossy().into_owned();
            let Ok(files) = fs::read_dir(shard.path()) else {
                continue;
            };
            for f in files {
                let f = f.at(&shard.path())?;
                let hash = format!("{prefix}{}", f.file_name().to_string_lossy());
                if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    let size = f.metadata().at(&f.path())?.len();
                    out.push((hash, f.path(), size));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChangeKind, diff, scan_dir};

    fn rand_bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn setup() -> (tempfile::TempDir, Store) {
        let t = tempfile::tempdir().unwrap();
        let s = Store::new(t.path().join(".pollard"));
        (t, s)
    }

    #[test]
    fn objects_roundtrip_and_layout() {
        let (_t, s) = setup();
        let h = s.put_object(b"hello").unwrap();
        assert_eq!(h, hash_bytes(b"hello"));
        assert!(
            s.root()
                .join("objects")
                .join(&h[..2])
                .join(&h[2..])
                .is_file()
        );
        assert_eq!(s.get_object(&h).unwrap(), b"hello");
        assert_eq!(s.put_object(b"hello").unwrap(), h);
        assert!(matches!(
            s.get_object(&hash_bytes(b"nope")),
            Err(Error::NotFound(_))
        ));
        // corruption is detected
        let p = s.root().join("objects").join(&h[..2]).join(&h[2..]);
        fs::write(&p, zstd::encode_all(&b"evil"[..], 3).unwrap()).unwrap();
        assert!(matches!(s.get_object(&h), Err(Error::Corrupt(_))));
    }

    #[test]
    fn large_file_chunked_roundtrip() {
        let (t, s) = setup();
        let data = rand_bytes(3 * 1024 * 1024 + 17, 1);
        let p = t.path().join("big.bin");
        fs::write(&p, &data).unwrap();
        let (h, size) = s.put_file(&p).unwrap();
        assert_eq!(
            (h.as_str(), size),
            (hash_bytes(&data).as_str(), data.len() as u64)
        );
        assert!(!s.has_object(&h) && s.has_blob(&h));
        let chunks = s.chunks_of(&h).unwrap().unwrap();
        assert!(chunks.len() > 10);
        assert!(chunks.iter().all(|(_, n)| *n <= CHUNK_MAX as u64));
        assert_eq!(chunks.iter().map(|(_, n)| n).sum::<u64>(), size);
        assert_eq!(s.read_blob(&h).unwrap(), data);
        assert_eq!(s.put_bytes(&data).unwrap(), (h, size));
    }

    #[test]
    fn snapshot_materialize_roundtrip() {
        let (t, s) = setup();
        let wc = t.path();
        let w = |rel: &str, body: &[u8]| {
            let p = wc.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        w("train.py", b"v1");
        w("model/net.py", b"net");
        w("REPORT.md", b"report v1");
        w("weights.bin", &rand_bytes(2 << 20, 2));
        fs::set_permissions(wc.join("train.py"), fs::Permissions::from_mode(0o755)).unwrap();
        let opts = WalkOptions::code(&["*.md".into()]);
        let a = s.snapshot_dir(wc, &opts).unwrap().manifest;
        assert_eq!(s.get_manifest(&a.hash()).unwrap(), a);

        w("train.py", b"v2");
        w("new/extra.py", b"x");
        fs::remove_dir_all(wc.join("model")).unwrap();
        w("REPORT.md", b"report v2");
        let b = s.snapshot_dir(wc, &opts).unwrap().manifest;
        let kinds: Vec<_> = diff(&a, &b).into_iter().map(|c| (c.path, c.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("model/net.py".into(), ChangeKind::Removed),
                ("new/extra.py".into(), ChangeKind::Added),
                ("train.py".into(), ChangeKind::Modified),
            ]
        );

        s.materialize(&a, wc, &opts).unwrap();
        assert_eq!(scan_dir(wc, &opts).unwrap().manifest, a);
        assert!(!wc.join("new").exists(), "emptied dir removed");
        assert_eq!(
            fs::read(wc.join("REPORT.md")).unwrap(),
            b"report v2",
            "off-tree untouched"
        );
        assert_eq!(
            fs::metadata(wc.join("train.py"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(s.root().join("objects").exists(), ".pollard untouched");

        // missing blob: refuse before touching anything
        let mut bad = a.clone();
        bad.entries[0].hash = hash_bytes(b"missing");
        assert!(matches!(
            s.materialize(&bad, wc, &opts),
            Err(Error::NotFound(_))
        ));
        assert_eq!(scan_dir(wc, &opts).unwrap().manifest, a);
    }

    #[test]
    fn gc_frees_only_unreachable_chunks() {
        let (t, s) = setup();
        let base = rand_bytes(4 << 20, 3);
        let mut other = base.clone();
        other[2 << 20..(2 << 20) + 4096].fill(7);
        let pa = t.path().join("a.bin");
        let pb = t.path().join("b.bin");
        fs::write(&pa, &base).unwrap();
        fs::write(&pb, &other).unwrap();
        let entry = |p: &Path| {
            let (hash, size) = s.put_file(p).unwrap();
            Entry {
                path: "ckpt.pt".into(),
                size,
                hash,
                mode: 0o100644,
            }
        };
        let (ea, eb) = (entry(&pa), entry(&pb));
        let ma = s.put_manifest(&Manifest::new(vec![ea.clone()])).unwrap();
        let mb = s.put_manifest(&Manifest::new(vec![eb.clone()])).unwrap();
        let ca: HashSet<_> = s
            .chunks_of(&ea.hash)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        let cb: HashSet<_> = s
            .chunks_of(&eb.hash)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        let unique_b = cb.difference(&ca).count();
        assert!(unique_b > 0 && unique_b < 5);

        assert_eq!(s.gc(&[ma.clone(), mb.clone()]).unwrap(), GcStats::default());
        let st = s.gc(std::slice::from_ref(&ma)).unwrap();
        assert_eq!((st.chunks_removed, st.maps_removed), (unique_b, 1));
        assert_eq!(s.read_blob(&ea.hash).unwrap(), base);
        assert!(s.gc(&[hash_bytes(b"no such manifest")]).is_err());
    }
}
