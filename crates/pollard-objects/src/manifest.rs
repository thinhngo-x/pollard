use std::cmp::Ordering;
use std::fs::Metadata;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use ignore::gitignore::GitignoreBuilder;
use ignore::overrides::OverrideBuilder;

use crate::{Error, IoCtx, Result, hash_bytes, hash_file};

pub const MODE_FILE: u32 = 0o100644;
pub const MODE_EXEC: u32 = 0o100755;
pub const MODE_LINK: u32 = 0o120000;

/// One manifest line. `hash` is blake3 of the file bytes (symlinks: of the link target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative, `/`-separated, UTF-8.
    pub path: String,
    pub size: u64,
    pub hash: String,
    /// Git-style: 0o100644, 0o100755 or 0o120000 (symlink).
    pub mode: u32,
}

/// Sorted list of entries; serialized as `path\tsize\thash\tmode(octal)\n` lines and hashed as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub entries: Vec<Entry>,
}

impl Manifest {
    pub fn new(mut entries: Vec<Entry>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        Manifest { entries }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{:o}\n",
                e.path, e.size, e.hash, e.mode
            ));
        }
        out.into_bytes()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::BadManifest {
            line: 0,
            reason: "not UTF-8".into(),
        })?;
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let bad = |reason: &str| Error::BadManifest {
                line: i + 1,
                reason: reason.into(),
            };
            let mut it = line.rsplitn(4, '\t');
            let (Some(mode), Some(hash), Some(size), Some(path)) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                return Err(bad("expected 4 tab-separated fields"));
            };
            entries.push(Entry {
                path: path.to_string(),
                size: size.parse().map_err(|_| bad("bad size"))?,
                hash: hash.to_string(),
                mode: u32::from_str_radix(mode, 8).map_err(|_| bad("bad mode"))?,
            });
        }
        Ok(Manifest::new(entries))
    }

    /// blake3 hex of the serialized manifest; this is the manifest's object id.
    pub fn hash(&self) -> String {
        hash_bytes(&self.to_bytes())
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }

    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

/// Changes going from `old` to `new`, sorted by path. A mode change counts as modified.
pub fn diff(old: &Manifest, new: &Manifest) -> Vec<Change> {
    let (mut a, mut b) = (old.entries.iter().peekable(), new.entries.iter().peekable());
    let mut out = Vec::new();
    let mut push = |path: &str, kind| {
        out.push(Change {
            path: path.to_string(),
            kind,
        })
    };
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(x), None) => {
                push(&x.path, ChangeKind::Removed);
                a.next();
            }
            (None, Some(y)) => {
                push(&y.path, ChangeKind::Added);
                b.next();
            }
            (Some(x), Some(y)) => match x.path.cmp(&y.path) {
                Ordering::Less => {
                    push(&x.path, ChangeKind::Removed);
                    a.next();
                }
                Ordering::Greater => {
                    push(&y.path, ChangeKind::Added);
                    b.next();
                }
                Ordering::Equal => {
                    if x.hash != y.hash || x.mode != y.mode {
                        push(&x.path, ChangeKind::Modified);
                    }
                    a.next();
                    b.next();
                }
            },
        }
    }
    out
}

/// What to include when walking a directory. `.pollard/` and `.git/` are always excluded.
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// gitignore-style globs relative to the root, applied as overrides:
    /// `!pat` excludes (e.g. off-tree files, output dirs); a plain `pat` whitelists,
    /// i.e. once any plain glob is present only matching files are walked (used for docs).
    /// A glob ending in `/` matches the directory and everything below it.
    pub globs: Vec<String>,
    /// Honour `.gitignore` and `.pollardignore` files inside the tree.
    pub ignore_files: bool,
    /// Files over this size are left out and reported in `Snapshot::oversized`.
    pub max_file_size: Option<u64>,
    /// Off-tree patterns with full gitignore semantics (last match wins, `!` re-includes,
    /// `dir/` covers everything below). Matching files are excluded, or with `offtree_only`
    /// they are the only files walked (docs manifests).
    pub offtree: Vec<String>,
    pub offtree_only: bool,
    /// Captured config belongs to code even when an off-tree pattern matches.
    pub config_file: Option<String>,
}

impl WalkOptions {
    /// Code manifest rules (§3): ignore files on, 10 MB limit, plus the given exclusions
    /// (bare patterns like `*.md`, `outputs/`; they are negated for you).
    pub fn code(excludes: &[String]) -> Self {
        WalkOptions {
            globs: excludes.iter().map(|g| format!("!{g}")).collect(),
            ignore_files: true,
            max_file_size: Some(crate::CODE_FILE_LIMIT),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub manifest: Manifest,
    /// `(path, size)` of files skipped for exceeding `max_file_size`.
    pub oversized: Vec<(String, u64)>,
}

/// Hash every file under `root` into a manifest without storing contents.
pub fn scan_dir(root: &Path, opts: &WalkOptions) -> Result<Snapshot> {
    build(root, opts, |abs, md| {
        if md.file_type().is_symlink() {
            let target = std::fs::read_link(abs).at(abs)?;
            let t = target.as_os_str().as_bytes();
            Ok((hash_bytes(t), t.len() as u64))
        } else {
            Ok((hash_file(abs)?, md.len()))
        }
    })
}

/// Walk `root` and build a manifest; `content` returns `(hash, size)` for a file or symlink.
pub(crate) fn build(
    root: &Path,
    opts: &WalkOptions,
    mut content: impl FnMut(&Path, &Metadata) -> Result<(String, u64)>,
) -> Result<Snapshot> {
    let mut snap = Snapshot::default();
    let mut entries = Vec::new();
    for (abs, rel, md) in list_files(root, opts)? {
        let link = md.file_type().is_symlink();
        if !link && opts.max_file_size.is_some_and(|max| md.len() > max) {
            snap.oversized.push((rel, md.len()));
            continue;
        }
        let (hash, size) = content(&abs, &md)?;
        entries.push(Entry {
            path: rel,
            size,
            hash,
            mode: mode_of(&md),
        });
    }
    snap.manifest = Manifest::new(entries);
    Ok(snap)
}

fn mode_of(md: &Metadata) -> u32 {
    if md.file_type().is_symlink() {
        MODE_LINK
    } else if md.permissions().mode() & 0o111 != 0 {
        MODE_EXEC
    } else {
        MODE_FILE
    }
}

/// Regular files and symlinks under `root` that pass the walk rules: `(abs, rel, metadata)`.
pub(crate) fn list_files(
    root: &Path,
    opts: &WalkOptions,
) -> Result<Vec<(PathBuf, String, Metadata)>> {
    let mut ob = OverrideBuilder::new(root);
    ob.add("!/.pollard/")?.add("!/.git/")?;
    for g in &opts.globs {
        ob.add(g)?;
        if g.ends_with('/') {
            ob.add(&format!("{g}**"))?;
        }
    }
    let mut wb = WalkBuilder::new(root);
    wb.hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .git_ignore(opts.ignore_files)
        .follow_links(false)
        .overrides(ob.build()?);
    if opts.ignore_files {
        wb.add_custom_ignore_filename(".pollardignore");
    }
    let mut gb = GitignoreBuilder::new(root);
    for g in &opts.offtree {
        gb.add_line(None, g)?;
    }
    let offtree = gb.build()?;
    let mut out = Vec::new();
    for ent in wb.build() {
        let ent = ent?;
        let abs = ent.path();
        let md = std::fs::symlink_metadata(abs).at(abs)?;
        if !(md.is_file() || md.file_type().is_symlink()) {
            continue;
        }
        let rel = abs.strip_prefix(root).unwrap_or(abs);
        let rel_str = rel
            .to_str()
            .filter(|s| !s.contains('\n'))
            .ok_or_else(|| Error::BadPath(abs.into()))?;
        let is_offtree = opts.config_file.as_deref() != Some(rel_str)
            && offtree.matched_path_or_any_parents(rel, false).is_ignore();
        if is_offtree != opts.offtree_only {
            continue;
        }
        out.push((abs.to_path_buf(), rel_str.to_string(), md));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn paths(s: &Snapshot) -> Vec<&str> {
        s.manifest.entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn roundtrip_and_hash_stable() {
        let m = Manifest::new(vec![
            Entry {
                path: "b\tweird".into(),
                size: 3,
                hash: hash_bytes(b"abc"),
                mode: MODE_EXEC,
            },
            Entry {
                path: "a/x.py".into(),
                size: 0,
                hash: hash_bytes(b""),
                mode: MODE_FILE,
            },
        ]);
        assert_eq!(m.entries[0].path, "a/x.py");
        let back = Manifest::parse(&m.to_bytes()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.hash(), m.hash());
        assert!(m.get("b\tweird").is_some());
        assert!(Manifest::parse(b"nope\n").is_err());
    }

    #[test]
    fn diff_kinds() {
        let e = |p: &str, h: &str| Entry {
            path: p.into(),
            size: 1,
            hash: h.into(),
            mode: MODE_FILE,
        };
        let a = Manifest::new(vec![e("a", "1"), e("b", "1"), e("c", "1")]);
        let b = Manifest::new(vec![e("b", "2"), e("c", "1"), e("d", "1")]);
        let d = diff(&a, &b);
        assert_eq!(
            d,
            vec![
                Change {
                    path: "a".into(),
                    kind: ChangeKind::Removed
                },
                Change {
                    path: "b".into(),
                    kind: ChangeKind::Modified
                },
                Change {
                    path: "d".into(),
                    kind: ChangeKind::Added
                },
            ]
        );
    }

    #[test]
    fn ignore_rules_and_excludes() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(r, "train.py", "print(1)");
        write(r, ".gitignore", "*.log\n");
        write(r, ".pollardignore", "scratch/\n");
        write(r, "run.log", "x");
        write(r, "scratch/tmp.py", "x");
        write(r, ".pollard/db.sqlite", "x");
        write(r, ".git/HEAD", "x");
        write(r, ".python-version", "3.10");
        write(r, "REPORT.md", "report");
        write(r, "notes/idea.txt", "idea");
        write(r, "outputs/o.bin", "o");
        write(r, "src/deep/mod.py", "x");
        write(r, "big.bin", &"x".repeat(100));
        let mut opts = WalkOptions::code(&["*.md".into(), "notes/".into(), "outputs/".into()]);
        opts.max_file_size = Some(50);
        let s = scan_dir(r, &opts).unwrap();
        assert_eq!(
            paths(&s),
            vec![
                ".gitignore",
                ".pollardignore",
                ".python-version",
                "src/deep/mod.py",
                "train.py"
            ]
        );
        assert_eq!(s.oversized, vec![("big.bin".to_string(), 100)]);

        // docs: whitelist the off-tree globs
        let docs = WalkOptions {
            globs: vec!["*.md".into(), "notes/".into()],
            ignore_files: true,
            ..Default::default()
        };
        assert_eq!(
            paths(&scan_dir(r, &docs).unwrap()),
            vec!["REPORT.md", "notes/idea.txt"]
        );

        // offtree with gitignore semantics: `!README.md` re-includes it in code
        write(r, "README.md", "readme");
        write(r, "src/deep/NOTES.md", "n");
        let off = vec!["*.md".to_string(), "notes/".into(), "!README.md".into()];
        let code = WalkOptions {
            offtree: off.clone(),
            ..WalkOptions::code(&["outputs/".into()])
        };
        let got = scan_dir(r, &code).unwrap();
        assert!(got.manifest.get("README.md").is_some());
        assert!(
            got.manifest.get("REPORT.md").is_none()
                && got.manifest.get("src/deep/NOTES.md").is_none()
        );
        assert!(
            got.manifest.get("notes/idea.txt").is_none() && got.manifest.get("train.py").is_some()
        );
        let docs = WalkOptions {
            offtree: off,
            offtree_only: true,
            ignore_files: true,
            ..Default::default()
        };
        assert_eq!(
            paths(&scan_dir(r, &docs).unwrap()),
            vec!["REPORT.md", "notes/idea.txt", "src/deep/NOTES.md"]
        );
    }

    #[test]
    fn modes_and_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(r, "run.sh", "#!/bin/sh");
        fs::set_permissions(r.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("run.sh", r.join("link")).unwrap();
        let s = scan_dir(r, &WalkOptions::default()).unwrap();
        let m = &s.manifest;
        assert_eq!(m.get("run.sh").unwrap().mode, MODE_EXEC);
        let l = m.get("link").unwrap();
        assert_eq!(
            (l.mode, l.hash.as_str(), l.size),
            (MODE_LINK, hash_bytes(b"run.sh").as_str(), 6)
        );
    }
}
