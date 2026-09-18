//! `pollard run`: snapshot → node (one op), then launch under the run protocol (§4).

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::delta::{self, Deltas};
use crate::node::{self, Node, Status};
use crate::wc::{self, Capture};
use crate::{Error, IoCtx, OpRecord, Repo, Result, env, metrics, msg, ops};

#[derive(Debug, Default, Clone)]
pub struct RunOpts {
    /// `-m` paragraphs (already read from stdin for `-m -`).
    pub notes: Vec<String>,
    pub parent: Option<String>,
    pub sweep: Option<String>,
    pub force: bool,
    pub strict: bool,
    pub cmd: Vec<String>,
}

/// What `start` prepared for `execute`.
#[derive(Debug, Clone)]
pub struct Launch {
    pub node: String,
    pub argv: Vec<String>,
    pub fork_step: Option<i64>,
    pub capture: Capture,
    /// Human summary for the "node … ← parent" line.
    pub parent: Option<String>,
    pub note_auto: Option<String>,
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run_dir(repo: &Repo, node: &str) -> PathBuf {
    repo.dot.join("runs").join(node)
}

pub fn ckpt_dir(repo: &Repo) -> PathBuf {
    repo.root.join(repo.config.checkpoint_dir.trim_start_matches("./"))
}

/// Snapshot the working copy and create the node (status `running`) as one op.
pub fn start(repo: &mut Repo, o: &RunOpts) -> Result<(OpRecord, Launch)> {
    if o.cmd.is_empty() {
        return Err(msg("run: no command given"));
    }
    let head = repo.head()?;
    // Parent: --parent, else the current node; sweep members stay siblings.
    let mut fork_step = repo.meta("fork_step")?.and_then(|s| s.parse::<i64>().ok());
    let parent = match &o.parent {
        Some(p) => {
            fork_step = None;
            Some(repo.resolve(p)?)
        }
        None => match head.as_deref().map(|h| repo.node(h)).transpose()? {
            Some(h) if o.sweep.is_some() && h.sweep == o.sweep => {
                fork_step = h.fork_step;
                h.parent.clone()
            }
            Some(h) => Some(h.id),
            None => None,
        },
    };
    let parent_node = parent.as_deref().map(|p| repo.node(p)).transpose()?;

    let argv = env::launch_command(&repo.root, &o.cmd);
    let lock_ok = env::lock_check(&repo.root);
    if lock_ok == Some(false) {
        if o.strict {
            return Err(msg(format!("uv.lock in {} is out of date (uv lock --check failed); refusing with --strict", repo.root.display())));
        }
        eprintln!("warning: uv.lock is out of date (uv lock --check failed)");
    }
    let env_hash = env::store(repo, &env::inputs(&repo.root, &o.cmd))?;

    // code
    let snap = repo.objects.snapshot_dir(&repo.root, &wc::code_opts(repo))?;
    for (p, size) in &snap.oversized {
        eprintln!("warning: {p} is {:.1} MB (> 10 MB); left out of the code manifest, treat it as data", *size as f64 / 1e6);
    }
    let manifest_hash = snap.manifest.hash();
    let code = wc::code_hash(repo, &snap.manifest)?;

    // config
    let capture = wc::capture_mode(repo);
    let mut cfg = match &capture {
        Capture::File(p) => {
            let path = repo.root.join(p);
            if path.is_file() { wc::read_config_file(&path)? } else { Value::Object(Default::default()) }
        }
        _ => Value::Object(Default::default()),
    };
    wc::apply_overrides(&mut cfg, &wc::overrides(&o.cmd[1.min(o.cmd.len())..]));
    let config = wc::store_config(repo, &cfg)?;

    let data = wc::data_manifest(repo)?;
    let docs_snap = repo.objects.snapshot_dir(&repo.root, &wc::docs_opts(repo))?;
    let docs = (!docs_snap.manifest.entries.is_empty()).then(|| docs_snap.manifest.hash());

    let recipe = node::recipe_hash(&code, &config, &data, &env_hash);
    // Duplicate check (§4): refuse running/done matches; others only get a notice.
    if !matches!(capture, Capture::Sdk | Capture::Hydra) {
        let dup: Option<(String, String)> = repo
            .db
            .query_row(
                "SELECT id, status FROM nodes WHERE recipe_hash=?1 AND command NOT LIKE 'pollard import %' ORDER BY status IN ('running','done') DESC LIMIT 1",
                [&recipe],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match dup {
            Some((d, st)) if (st == "running" || st == "done") && !o.force => return Err(Error::Duplicate(d)),
            Some((d, st)) if st != "running" && st != "done" => {
                eprintln!("note: same recipe as {d} ({st}); running again")
            }
            _ => {}
        }
    }

    // deltas against the parent
    let deltas = match &parent_node {
        Some(p) => compute_deltas(repo, p, &snap.manifest, &cfg, &data, &env_hash)?,
        None => Deltas::default(),
    };
    let added = deltas.code.iter().filter(|c| c.kind == pollard_objects::ChangeKind::Added).count();
    if added > 500 {
        eprintln!("warning: {added} new files since the parent; add build/output dirs to .pollardignore or output_dirs");
    }

    let (note, note_auto) = if o.notes.is_empty() {
        (delta::auto_note(&deltas, wc::config_file(repo).as_deref()), true)
    } else {
        (o.notes.join("\n\n"), false)
    };

    let id = repo.next_id()?;
    let n = Node {
        id: id.clone(),
        parent: parent.clone(),
        code,
        config,
        data,
        env: env_hash,
        recipe_hash: recipe,
        weights: None,
        docs,
        fork_step,
        status: Status::Running,
        created_at: crate::now(),
        finished_at: None,
        command: shell_join(&argv),
        note: Some(note.clone()),
        note_auto,
        lock_ok,
        depth: parent_node.as_ref().map_or(0, |p| p.depth + 1),
        sweep: o.sweep.clone(),
    };
    let rec = ops::record(repo, Some(manifest_hash), |repo| {
        n.insert(&repo.db)?;
        delta::store(repo, &n.id, &deltas)?;
        repo.set_head(Some(&n.id))?;
        repo.del_meta("fork_step")?;
        Ok(n.id.clone())
    })?;
    let launch = Launch { node: id, argv, fork_step, capture, parent, note_auto: note_auto.then_some(note) };
    Ok((rec, launch))
}

pub fn compute_deltas(
    repo: &Repo,
    parent: &Node,
    code: &pollard_objects::Manifest,
    cfg: &Value,
    data: &str,
    env_hash: &str,
) -> Result<Deltas> {
    // A parent without a stored code manifest (e.g. imported from git) gets no code delta.
    let pcode = match wc::manifest_of(repo, &parent.code) {
        Ok(h) => Some(repo.objects.get_manifest(&h)?),
        Err(_) => None,
    };
    let code_delta = pcode.map(|pc| pollard_objects::diff(&pc, code)).unwrap_or_default();
    let pcfg = wc::load_config(repo, &parent.config)?;
    let pdata = repo.objects.get_manifest(&parent.data)?;
    let ndata = repo.objects.get_manifest(data)?;
    let changes = pollard_objects::diff(&pdata, &ndata);
    let data_delta = delta::DataDelta {
        files_added: changes.iter().filter(|c| c.kind == pollard_objects::ChangeKind::Added).count() as u64,
        files_removed: changes.iter().filter(|c| c.kind == pollard_objects::ChangeKind::Removed).count() as u64,
        bytes_delta: ndata.total_size() as i64 - pdata.total_size() as i64,
        changes,
    };
    Ok(Deltas {
        config: delta::config_delta(&pcfg, cfg),
        code: code_delta,
        data: data_delta,
        env: env::delta(repo, &parent.env, env_hash),
    })
}

/// Incremental reader for the metrics JSONL: whole lines only, remembers its offset.
struct Tail {
    path: PathBuf,
    pos: u64,
    line_no: usize,
}

impl Tail {
    fn poll(&mut self, repo: &Repo, node: &str, flush: bool) -> Result<usize> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return Ok(0) };
        f.seek(SeekFrom::Start(self.pos)).at(&self.path)?;
        let mut r = BufReader::new(f);
        let mut n = 0;
        repo.db.execute_batch("BEGIN")?;
        let mut buf = String::new();
        loop {
            buf.clear();
            let got = r.read_line(&mut buf).at(&self.path)?;
            if got == 0 || (!buf.ends_with('\n') && !flush) {
                break;
            }
            self.pos += got as u64;
            self.line_no += 1;
            let k = metrics::ingest_line(&repo.db, node, &buf)?;
            if k == 0 && !buf.trim().is_empty() {
                eprintln!("warning: {}:{}: skipped malformed metrics line", self.path.display(), self.line_no);
            }
            n += k;
        }
        repo.db.execute_batch("COMMIT")?;
        Ok(n)
    }
}

/// Launch the command with the protocol env vars, tail metrics, set status on exit.
/// Returns the child's exit code.
pub fn execute(repo: &Repo, l: &Launch) -> Result<i32> {
    let dir = run_dir(repo, &l.node);
    std::fs::create_dir_all(&dir).at(&dir)?;
    let ckpt = ckpt_dir(repo);
    std::fs::create_dir_all(&ckpt).at(&ckpt)?;
    let metrics_path = dir.join("metrics.jsonl");
    let config_path = dir.join("config.json");

    let mut cmd = Command::new(&l.argv[0]);
    cmd.args(&l.argv[1..])
        .env("POLLARD_NODE_ID", &l.node)
        .env("POLLARD_METRICS", &metrics_path)
        .env("POLLARD_CKPT_DIR", &ckpt)
        .env("POLLARD_CONFIG", &config_path);
    match l.fork_step {
        Some(s) => cmd.env("POLLARD_FORK_STEP", s.to_string()),
        None => cmd.env_remove("POLLARD_FORK_STEP"),
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        // Ctrl-C reaches the child through the process group; we just survive to record it.
        let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    }
    let ckpt_before = crate::weights::ckpt_state(repo)?;
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            finish(repo, &l.node, Status::Failed)?;
            return Err(msg(format!("{}: failed to launch: {e}", l.argv[0])));
        }
    };
    let mut tail = Tail { path: metrics_path, pos: 0, line_no: 0 };
    let mut config_done = !matches!(l.capture, Capture::Sdk);
    // Poll metrics every 200 ms (D-9); check for exit every 20 ms so short runs return fast.
    let mut tick = 0u32;
    let status = loop {
        if let Some(st) = child.try_wait().at(&l.argv[0])? {
            break st;
        }
        if tick % 10 == 0 {
            if !config_done && config_path.is_file() {
                config_done = capture_sdk_config(repo, &l.node, &config_path)?;
            }
            tail.poll(repo, &l.node, false)?;
        }
        tick += 1;
        std::thread::sleep(Duration::from_millis(20));
    };
    if !config_done && config_path.is_file() {
        capture_sdk_config(repo, &l.node, &config_path)?;
    }
    if l.capture == Capture::Hydra {
        capture_hydra_config(repo, &l.node)?;
    }
    tail.poll(repo, &l.node, true)?;
    crate::weights::register_new(repo, &l.node, &ckpt_before)?;
    use std::os::unix::process::ExitStatusExt;
    let st = if status.success() {
        Status::Done
    } else if status.signal().is_some() || interrupted.load(Ordering::SeqCst) || status.code() == Some(130) {
        Status::Killed
    } else {
        Status::Failed
    };
    finish(repo, &l.node, st)?;
    Ok(status.code().unwrap_or(128 + status.signal().unwrap_or(0)))
}

fn finish(repo: &Repo, node: &str, st: Status) -> Result<()> {
    let _lock = repo.lock()?;
    repo.db.execute(
        "UPDATE nodes SET status=?1, finished_at=?2 WHERE id=?3",
        params![st.as_str(), crate::now(), node],
    )?;
    Ok(())
}

/// Replace a running node's config (sdk/hydra capture) and recompute its recipe and config delta.
fn set_config(repo: &Repo, node_id: &str, cfg: &Value) -> Result<()> {
    let _lock = repo.lock()?;
    let n = repo.node(node_id)?;
    let config = wc::store_config(repo, cfg)?;
    let recipe = node::recipe_hash(&n.code, &config, &n.data, &n.env);
    let dup: Option<String> = repo
        .db
        .query_row(
            "SELECT id FROM nodes WHERE recipe_hash=?1 AND status!='pruned' AND id!=?2 LIMIT 1",
            [&recipe, node_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(d) = dup {
        eprintln!("warning: {node_id} has the same recipe as {d}");
    }
    repo.db.execute("UPDATE nodes SET config=?1, recipe_hash=?2 WHERE id=?3", params![config, recipe, node_id])?;
    if let Some(p) = &n.parent {
        let mut d = delta::load(repo, node_id)?;
        d.config = delta::config_delta(&wc::load_config(repo, &repo.node(p)?.config)?, cfg);
        delta::store(repo, node_id, &d)?;
        if n.note_auto && !d.config.is_empty() {
            repo.db.execute("UPDATE nodes SET note=?1 WHERE id=?2", params![delta::auto_note(&d, wc::config_file(repo).as_deref()), node_id])?;
        }
    }
    Ok(())
}

fn capture_sdk_config(repo: &Repo, node: &str, path: &Path) -> Result<bool> {
    let Ok(mut cfg) = wc::read_config_file(path) else { return Ok(false) }; // partially written
    let n = repo.node(node)?;
    wc::apply_overrides(&mut cfg, &wc::overrides(&shell_words(&n.command)));
    set_config(repo, node, &cfg)?;
    Ok(true)
}

/// Newest `.hydra/config.yaml` under the output dirs.
fn capture_hydra_config(repo: &Repo, node: &str) -> Result<()> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for d in &repo.config.output_dirs {
        for ent in ignore::WalkBuilder::new(repo.root.join(d)).standard_filters(false).build().flatten() {
            let p = ent.path();
            if p.ends_with(".hydra/config.yaml") {
                if let Ok(t) = p.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().is_none_or(|b| t > b.0) {
                        best = Some((t, p.to_path_buf()));
                    }
                }
            }
        }
    }
    if let Some((_, p)) = best {
        set_config(repo, node, &wc::read_config_file(&p)?)?;
    }
    Ok(())
}

/// Split a command stored by `shell_join` back into words.
fn shell_words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut q, mut any) = (vec![], String::new(), false, false);
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' if !q => {
                cur.extend(it.next());
                any = true;
            }
            '\'' => {
                q = !q;
                any = true;
            }
            ' ' if !q => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            _ => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn quoting_roundtrip() {
        let argv: Vec<String> = ["uv", "run", "train.py", "seed=1", "it's here", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::shell_words(&super::shell_join(&argv)), argv);
    }
}
