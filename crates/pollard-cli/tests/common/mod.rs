//! Shared fixtures for the pollard integration suite.
//!
//! Every test gets a fresh temp dir laid out as:
//!   <tmp>/wc/   the working copy (the pollard repo root)
//!   <tmp>/aux/  scripts and fixtures that must NOT be part of any recipe
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("pollard")
}

#[derive(Debug)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
    pub elapsed: Duration,
    pub cmdline: String,
}

impl Out {
    /// Last non-empty stdout line, trimmed. §4/§10: mutating commands print the node id here.
    pub fn last_line(&self) -> String {
        self.stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string()
    }
    pub fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Display for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "$ {}\n[exit {:?}, {:?}]\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.cmdline, self.code, self.elapsed, self.stdout, self.stderr
        )
    }
}

pub struct Repo {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub aux: PathBuf,
}

impl Repo {
    /// Empty working copy, not initialised.
    pub fn bare() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wc");
        let aux = tmp.path().join("aux");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&aux).unwrap();
        Repo { tmp, root, aux }
    }

    /// Working copy with `config.yaml` + `train.sh`, then `pollard init`.
    pub fn init() -> Repo {
        let r = Repo::bare();
        r.write("config.yaml", "lr: 3\ndepth: 12\nact: relu\n");
        r.write("model.py", "x = 1\n");
        r.ok(&["init"]);
        r
    }

    pub fn cmd_in(&self, dir: &Path, program: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(program);
        c.args(args).current_dir(dir);
        for (k, _) in std::env::vars() {
            if k.starts_with("POLLARD_") {
                c.env_remove(k);
            }
        }
        // The built binary is on PATH so scripts / the Python SDK can call `pollard`.
        let path = format!(
            "{}:{}",
            bin().parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        c.env("PATH", path)
            .env("POLLARD_BIN", bin())
            .env("EDITOR", "true")
            .env("VISUAL", "true")
            .env("NO_COLOR", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null") // user config may force gpg signing
            .stdin(Stdio::null());
        c
    }

    pub fn exec(&self, mut c: Command) -> Out {
        let cmdline = format!("{:?}", c);
        let t = Instant::now();
        let o = c.output().expect("spawn");
        Out {
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
            code: o.status.code(),
            elapsed: t.elapsed(),
            cmdline,
        }
    }

    /// Run `pollard <args>` in the working copy.
    pub fn po(&self, args: &[&str]) -> Out {
        self.exec(self.cmd_in(&self.root, &bin(), args))
    }

    /// `pollard <args>` expecting exit 0.
    pub fn ok(&self, args: &[&str]) -> Out {
        let o = self.po(args);
        assert!(o.ok(), "expected success:\n{o}");
        o
    }

    /// `pollard <args>` expecting non-zero exit.
    pub fn fail(&self, args: &[&str]) -> Out {
        let o = self.po(args);
        assert!(!o.ok(), "expected failure:\n{o}");
        o
    }

    /// Any external program (git, uv, sh) in the working copy, expecting success.
    pub fn sh(&self, script: &str) -> Out {
        let o = self.exec(self.cmd_in(&self.root, Path::new("sh"), &["-c", script]));
        assert!(o.ok(), "shell failed:\n{o}");
        o
    }

    pub fn write(&self, rel: &str, s: impl AsRef<[u8]>) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }
    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }
    pub fn rm(&self, rel: &str) {
        let p = self.root.join(rel);
        if p.is_dir() {
            fs::remove_dir_all(p).unwrap()
        } else {
            fs::remove_file(p).unwrap()
        }
    }

    /// Write an executable sh script into aux/ (outside the recipe) and return its path.
    pub fn script(&self, name: &str, body: &str) -> String {
        let p = self.aux.join(name);
        fs::write(&p, format!("#!/bin/sh\nset -e\n{}\n{body}\n", SH_PRELUDE)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.to_string_lossy().into_owned()
    }

    /// `pollard run <flags> -- sh <script>`; asserts success and returns the node id (last line).
    pub fn run(&self, flags: &[&str], body: &str) -> String {
        let o = self.run_out(flags, body);
        assert!(o.ok(), "run failed:\n{o}");
        let id = o.last_line();
        assert!(
            is_node_id(&id),
            "last line of `run` is not a node id: {id:?}\n{o}"
        );
        id
    }

    pub fn run_out(&self, flags: &[&str], body: &str) -> Out {
        let n = SCRIPT_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let s = self.script(&format!("run{n}.sh"), body);
        let mut args: Vec<&str> = vec!["run"];
        args.extend_from_slice(flags);
        args.extend_from_slice(&["--", "sh", &s]);
        self.po(&args)
    }

    /// Id of the current node: first node-id-shaped token of `show @`.
    pub fn current(&self) -> String {
        first_id(&self.ok(&["show", "@"]).stdout).expect("no node id in `show @`")
    }

    /// Snapshot of every file under the working copy (excluding .pollard/, .git/) → (path, bytes).
    pub fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        let mut v = vec![];
        walk(&self.root, &self.root, &mut v);
        v.sort();
        v
    }

    /// Set a top-level key in .pollard/config.toml (replacing any existing line for it).
    pub fn set_config(&self, key: &str, toml_value: &str) {
        let p = self.root.join(".pollard/config.toml");
        let s = fs::read_to_string(&p).unwrap_or_default();
        let mut lines: Vec<String> = s
            .lines()
            .filter(|l| {
                let t = l.trim_start_matches(['#', ' ']);
                !(t.starts_with(key) && t[key.len()..].trim_start().starts_with('='))
            })
            .map(String::from)
            .collect();
        // top-level keys must precede any [table]
        let at = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        lines.insert(at, format!("{key} = {toml_value}"));
        fs::write(&p, lines.join("\n") + "\n").unwrap();
    }

    /// Number of chunk files under .pollard/chunks.
    pub fn chunk_count(&self) -> usize {
        count_files(&self.root.join(".pollard/chunks"))
    }
}

static SCRIPT_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Shell helpers available to every run script.
/// `emit STEP k=v ...` appends one JSONL line to $POLLARD_METRICS.
pub const SH_PRELUDE: &str = r#"
emit() { s=$1; shift; line="{\"step\": $s"; for kv in "$@"; do line="$line, \"${kv%%=*}\": ${kv#*=}"; done; echo "$line}" >> "$POLLARD_METRICS"; }
"#;

fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
        if rel == ".pollard" || rel == ".git" {
            continue;
        }
        if p.is_dir() {
            walk(base, &p, out)
        } else {
            out.push((rel, fs::read(&p).unwrap()))
        }
    }
}

pub fn count_files(dir: &Path) -> usize {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.map(|e| {
        let p = e.unwrap().path();
        if p.is_dir() { count_files(&p) } else { 1 }
    })
    .sum()
}

/// Node ids look like `warm-fox-7`: `<adjective>-<noun>-<counter>` (v3 §3; counter is an integer).
pub fn is_node_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 3
        && parts[..2]
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()))
        && !parts[2].is_empty()
        && parts[2].chars().all(|c| c.is_ascii_digit())
}

pub fn ids_in(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|t| is_node_id(t))
        .map(String::from)
        .collect()
}

pub fn first_id(text: &str) -> Option<String> {
    ids_in(text).into_iter().next()
}

/// Line of `text` containing `needle`.
pub fn line_with<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    text.lines().find(|l| l.contains(needle))
}

/// Column where `needle` starts on its line (chars), for indentation checks in `tree`.
pub fn col_of(text: &str, needle: &str) -> usize {
    let l = line_with(text, needle).unwrap_or_else(|| panic!("{needle} not in:\n{text}"));
    l[..l.find(needle).unwrap()].chars().count()
}

/// All numbers appearing in a string (handles `−` U+2212 as minus, `2.19(−.16)` etc).
pub fn numbers(s: &str) -> Vec<f64> {
    let s = s.replace('−', "-");
    let mut out = vec![];
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i] == '-' || b[i] == '+' {
            i += 1;
        }
        let ds = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.' || b[i] == 'e' && i > ds) {
            i += 1;
        }
        let tok: String = b[start..i].iter().collect();
        if i > ds {
            if let Ok(v) = tok.trim_end_matches(['.', 'e']).parse::<f64>() {
                out.push(v)
            }
        } else {
            i = start + 1;
        }
    }
    out
}

pub fn has_num(s: &str, v: f64) -> bool {
    numbers(s)
        .iter()
        .any(|x| (x - v).abs() < 1e-6 * v.abs().max(1.0))
}

/// Deterministic pseudo-random bytes (xorshift64), used for fake checkpoints.
pub fn prng_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// Timing budget for a spec'd limit. The spec's numbers are for a release build (PLAN C5), so a
/// debug test build (tests share the binary's profile) gets 5x slack; `cargo test --release` checks
/// the real number.
pub fn budget(spec: Duration) -> Duration {
    if cfg!(debug_assertions) {
        spec * 5
    } else {
        spec
    }
}

pub const MB: usize = 1024 * 1024;

/// Fake 50 MB checkpoint (§8 "a fake 50 MB checkpoint").
pub fn fake_ckpt(seed: u64) -> Vec<u8> {
    prng_bytes(50 * MB, seed)
}

/// Overwrite `frac` of the bytes in `n` contiguous regions (localised edits, like a fine-tune
/// touching a few tensors). Returns the modified copy.
pub fn perturb(data: &[u8], frac: f64, n: usize, seed: u64) -> Vec<u8> {
    let mut d = data.to_vec();
    let region = ((d.len() as f64 * frac) as usize) / n;
    let noise = prng_bytes(region, seed ^ 0xABCD);
    for i in 0..n {
        let at = (d.len() / n) * i + (d.len() / n - region) / 2;
        for (j, b) in noise.iter().enumerate() {
            d[at + j] = !*b ^ (i as u8);
        }
    }
    d
}

/// A 200-file source tree (§9 M1).
pub fn write_200_files(r: &Repo) {
    for i in 0..200 {
        r.write(
            &format!("src/pkg{}/mod{i}.py", i % 10),
            format!(
                "# module {i}\ndef f{i}(x):\n    return x * {i}\n{}",
                "#".repeat(i * 20)
            ),
        );
    }
}

/// git helper in the working copy.
pub fn git(r: &Repo, args: &str) -> String {
    r.sh(&format!("git {args}")).stdout.trim().to_string()
}

pub fn git_init_commit(r: &Repo) {
    r.sh("git init -q -b main . && git add -A && git commit -q -m init");
}

// ---------------------------------------------------------------------------------------------
// Phase 1 (format 2) helpers: the alpha.1 fixture (BACKLOG F0), sqlite, file-state snapshots.
// ---------------------------------------------------------------------------------------------

/// `tests/fixtures/alpha1/` (make.sh, repo.tar.gz, remote.tar.gz, golden/).
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alpha1")
}

/// A file from the fixture's `golden/` (alpha.1's own output).
pub fn golden(name: &str) -> String {
    fs::read_to_string(fixture_dir().join("golden").join(name))
        .unwrap_or_else(|e| panic!("golden/{name}: {e}"))
}

/// The published alpha.1 binary, from `POLLARD_ALPHA1_BIN`. `None` (with a note on stderr)
/// when unset, so CI-only tests skip locally. Usage:
/// `let Some(a1) = alpha1_bin("test_name") else { return };`
pub fn alpha1_bin(test: &str) -> Option<PathBuf> {
    match std::env::var_os("POLLARD_ALPHA1_BIN").map(PathBuf::from) {
        Some(p) if p.is_file() => Some(p),
        other => {
            eprintln!(
                "SKIP {test}: needs the published alpha.1 binary; set POLLARD_ALPHA1_BIN \
                 (cargo install pollard-cli --version 0.1.0-alpha.1 --locked --root <dir>) [got {other:?}]"
            );
            None
        }
    }
}

/// A fresh copy of the alpha.1 fixture: `<tmp>/repo` (the working copy, `remote = "../remote"`)
/// and `<tmp>/remote` (the legacy alpha.1 remote).
pub struct Fixture {
    pub repo: Repo,
    pub remote: PathBuf,
    ids: std::collections::BTreeMap<String, String>,
}

impl Fixture {
    pub fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        for t in ["repo.tar.gz", "remote.tar.gz"] {
            let o = Command::new("tar")
                .arg("xzf")
                .arg(fixture_dir().join(t))
                .arg("-C")
                .arg(tmp.path())
                .output()
                .expect("tar");
            assert!(o.status.success(), "untar {t}: {o:?}");
        }
        let root = tmp.path().join("repo");
        let aux = tmp.path().join("aux");
        let remote = tmp.path().join("remote");
        fs::create_dir_all(&aux).unwrap();
        let ids = serde_json::from_str(&golden("ids.json")).unwrap();
        Fixture {
            repo: Repo { tmp, root, aux },
            remote,
            ids,
        }
    }

    /// Node id of a fixture role (`root`, `base`, `mid`, `best`, `crashed`, `stopped`, `late`,
    /// `kept`, `ghost`, `redo`).
    pub fn id(&self, role: &str) -> String {
        self.ids
            .get(role)
            .unwrap_or_else(|| panic!("no role {role} in golden/ids.json"))
            .clone()
    }

    pub fn roles(&self) -> Vec<(String, String)> {
        self.ids
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Timestamp of the newest op whose command is exactly `cmd` (e.g. `pollard prune <id>`),
    /// from alpha.1's `op log` in golden/.
    pub fn op_ts(cmd: &str) -> String {
        golden("op_log.txt")
            .lines()
            .find_map(|l| {
                let mut it = l.split_whitespace();
                let _n = it.next()?;
                let ts = it.next()?;
                let rest: Vec<&str> = it.collect();
                (rest.join(" ") == cmd).then(|| ts.to_string())
            })
            .unwrap_or_else(|| panic!("no op `{cmd}` in golden/op_log.txt"))
    }

    /// A second working copy of the unmigrated fixture repo at `<tmp>/<name>` sharing the same
    /// remote (`../remote`), e.g. an alpha.1 clone next to one that gets migrated.
    pub fn copy_repo(&self, name: &str) -> Repo {
        let dst = self.repo.tmp.path().join(name);
        let o = Command::new("cp")
            .arg("-a")
            .arg(&self.repo.root)
            .arg(&dst)
            .output()
            .unwrap();
        assert!(o.status.success(), "cp: {o:?}");
        let aux = self.repo.tmp.path().join(format!("{name}-aux"));
        fs::create_dir_all(&aux).unwrap();
        // Repo owns a TempDir; give the copy its own (empty) one and point root at the copy.
        Repo {
            tmp: tempfile::tempdir().unwrap(),
            root: dst,
            aux,
        }
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture::new()
    }
}

impl Repo {
    /// `pollard <args>` with extra env vars (set after cmd_in strips POLLARD_*).
    pub fn po_env(&self, env: &[(&str, &str)], args: &[&str]) -> Out {
        let mut c = self.cmd_in(&self.root, &bin(), args);
        for (k, v) in env {
            c.env(k, v);
        }
        self.exec(c)
    }

    /// Run another pollard binary (e.g. the published alpha.1) in this working copy.
    pub fn po_with(&self, program: &Path, args: &[&str]) -> Out {
        self.exec(self.cmd_in(&self.root, program, args))
    }

    /// The format-2 database (`.pollard/state.sqlite`).
    pub fn state_db(&self) -> PathBuf {
        self.root.join(".pollard/state.sqlite")
    }

    /// `sqlite3` query against the format-2 database; `|`-separated rows.
    pub fn sql(&self, q: &str) -> String {
        sql(&self.state_db(), q)
    }

    /// `PRAGMA user_version` of `.pollard/state.sqlite`.
    pub fn user_version(&self) -> i64 {
        self.sql("PRAGMA user_version;").trim().parse().unwrap()
    }

    /// `(status, pruned_at or "")` of one node, from the format-2 database.
    pub fn status_of(&self, id: &str) -> (String, String) {
        let row = self.sql(&format!(
            "SELECT status, coalesce(pruned_at, '') FROM nodes WHERE id='{id}';"
        ));
        let (s, p) = row
            .trim()
            .split_once('|')
            .unwrap_or_else(|| panic!("node {id} not in state.sqlite: {row:?}"));
        (s.to_string(), p.to_string())
    }

    /// Byte-and-mtime state of every file and directory under `.pollard/`.
    pub fn dot_state(&self) -> std::collections::BTreeMap<String, FileState> {
        file_state(&self.root.join(".pollard"))
    }
}

/// Run `sqlite3 -batch <db> <q>` and return stdout; panics on error.
pub fn sql(db: &Path, q: &str) -> String {
    let o = Command::new("sqlite3")
        .arg("-batch")
        .arg(db)
        .arg(q)
        .output()
        .expect("sqlite3 must be installed for the phase-1 tests");
    assert!(
        o.status.success(),
        "sqlite3 {} {q:?}: {}",
        db.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `sqlite3` that may fail: `(ok, stdout+stderr)`.
pub fn sql_try(db: &Path, q: &str) -> (bool, String) {
    let o = Command::new("sqlite3")
        .arg("-batch")
        .arg(db)
        .arg(q)
        .output()
        .expect("sqlite3");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Dir,
    File {
        bytes: Vec<u8>,
        mtime: Option<std::time::SystemTime>,
    },
}

/// Every file (bytes + mtime) and directory under `dir`, keyed by relative path.
pub fn file_state(dir: &Path) -> std::collections::BTreeMap<String, FileState> {
    fn go(base: &Path, d: &Path, m: &mut std::collections::BTreeMap<String, FileState>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            if p.is_dir() {
                m.insert(rel, FileState::Dir);
                go(base, &p, m);
            } else {
                let mtime = p.metadata().ok().and_then(|x| x.modified().ok());
                m.insert(
                    rel,
                    FileState::File {
                        bytes: fs::read(&p).unwrap_or_default(),
                        mtime,
                    },
                );
            }
        }
    }
    let mut m = std::collections::BTreeMap::new();
    go(dir, dir, &mut m);
    m
}

/// Human-readable difference between two `file_state` snapshots (empty = identical).
pub fn state_diff(
    a: &std::collections::BTreeMap<String, FileState>,
    b: &std::collections::BTreeMap<String, FileState>,
) -> String {
    let mut out = String::new();
    for (k, v) in a {
        match b.get(k) {
            None => out += &format!("removed {k}\n"),
            Some(w) if w != v => out += &format!("changed {k}\n"),
            _ => {}
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            out += &format!("added {k}\n");
        }
    }
    out
}

/// Same, ignoring mtimes and SQLite's transient `-wal`/`-shm` files: "changes nothing" in
/// content terms.
pub fn content_diff(
    a: &std::collections::BTreeMap<String, FileState>,
    b: &std::collections::BTreeMap<String, FileState>,
) -> String {
    let strip = |m: &std::collections::BTreeMap<String, FileState>| {
        m.iter()
            .filter(|(k, _)| !k.ends_with("-wal") && !k.ends_with("-shm"))
            .map(|(k, v)| {
                let v = match v {
                    FileState::File { bytes, .. } => FileState::File {
                        bytes: bytes.clone(),
                        mtime: None,
                    },
                    d => d.clone(),
                };
                (k.clone(), v)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    state_diff(&strip(a), &strip(b))
}

/// A format-2 working copy (`Repo::init`) whose config points at `remote`.
pub fn init_with_remote(remote: &Path) -> Repo {
    let r = Repo::init();
    r.set_config("remote", &format!("{:?}", remote.display().to_string()));
    r
}

/// Drop the ` (@)` marker so trees from different clones compare equal.
pub fn strip_at(tree: &str) -> String {
    tree.replace(" (@)", "")
}

/// Every node id in `.pollard/state.sqlite`, sorted.
pub fn node_ids(r: &Repo) -> Vec<String> {
    r.sql("SELECT id FROM nodes ORDER BY id;")
        .lines()
        .map(String::from)
        .collect()
}

/// Byte-for-byte difference (every file, `-wal`/`-shm` included; mtimes ignored).
pub fn bytes_diff(
    a: &std::collections::BTreeMap<String, FileState>,
    b: &std::collections::BTreeMap<String, FileState>,
) -> String {
    let strip = |m: &std::collections::BTreeMap<String, FileState>| {
        m.iter()
            .map(|(k, v)| {
                let v = match v {
                    FileState::File { bytes, .. } => FileState::File {
                        bytes: bytes.clone(),
                        mtime: None,
                    },
                    d => d.clone(),
                };
                (k.clone(), v)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    state_diff(&strip(a), &strip(b))
}
