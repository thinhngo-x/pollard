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
