//! Small mutating commands: `note`, `pin`, `unpin`, `fork`, `apply`; and `diff` rendering.

use std::fmt::Write as _;

use pollard_objects::{ChangeKind, Manifest};
use rusqlite::{OptionalExtension, params};

use crate::delta::{self, fmt_change};
use crate::{OpRecord, Repo, Result, env, metrics, msg, ops, siblings, wc};

pub fn note(repo: &mut Repo, node: &str, text: &str) -> Result<OpRecord> {
    let id = repo.resolve(node)?;
    ops::record(repo, None, |repo| {
        repo.db.execute(
            "UPDATE nodes SET note=?1, note_auto=0 WHERE id=?2",
            params![text, id],
        )?;
        Ok(id.clone())
    })
}

pub fn pin(repo: &mut Repo, node: &str, name: &str) -> Result<OpRecord> {
    let id = repo.resolve(node)?;
    if name.is_empty() || name.starts_with('@') {
        return Err(msg(format!("invalid pin name '{name}'")));
    }
    ops::record(repo, None, |repo| {
        repo.db.execute(
            "INSERT OR REPLACE INTO pins(name,node_id) VALUES(?1,?2)",
            params![name, id],
        )?;
        Ok(id.clone())
    })
}

pub fn unpin(repo: &mut Repo, name: &str) -> Result<OpRecord> {
    let id: String = repo
        .db
        .query_row("SELECT node_id FROM pins WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| msg(format!("no pin named '{name}'")))?;
    ops::record(repo, None, |repo| {
        repo.db.execute("DELETE FROM pins WHERE name=?1", [name])?;
        Ok(id.clone())
    })
}

/// Result of `fork`, for the CLI to report.
pub struct Forked {
    pub op: OpRecord,
    /// one-line notices (fork-step re-parent, inexact checkpoint step)
    pub notices: Vec<String>,
    /// checkpoint restored for `--step`: (path, step)
    pub restored: Option<(String, i64)>,
    pub fork_step: Option<i64>,
    pub synced: bool,
}

/// Restore `node`'s code and config into the working copy and make it current.
/// With `step`, restore the checkpoint (largest step <= N) and have the next run record
/// `fork_step`; a re-parent under the §5 rule behaves exactly like forking the ancestor.
/// The working copy is saved to the op log first.
pub fn fork(repo: &mut Repo, node: &str, step: Option<i64>, no_sync: bool) -> Result<Forked> {
    let target = repo.node(&repo.resolve(node)?)?;
    let (anchor, notice) = match step {
        Some(s) => metrics::fork_target(&repo.db, target, s)?,
        None => (target, None),
    };
    let manifest = wc::manifest_of(repo, &anchor.code)?;
    let mut notices: Vec<String> = notice.into_iter().collect();
    let mut fork_step = step;
    let restored = match step {
        Some(s) => {
            let r = crate::weights::restore_step(repo, &anchor.id, s)?;
            match &r {
                Some((p, got)) if *got != s => {
                    notices.push(format!("note: no checkpoint at step {s}; restored {p} (step {got}) and fork_step={got}"));
                    fork_step = Some(*got);
                }
                None => notices.push(format!(
                    "note: {} has no checkpoint at or before step {s}; nothing restored",
                    anchor.id
                )),
                _ => {}
            }
            r
        }
        None => None,
    };
    let saved = wc::snapshot_manifest(repo)?;
    let op = ops::record(repo, Some(saved), |repo| {
        repo.set_head(Some(&anchor.id))?;
        match fork_step {
            Some(s) => repo.set_meta("fork_step", &s.to_string())?,
            None => repo.del_meta("fork_step")?,
        }
        Ok(anchor.id.clone())
    })?;
    wc::checkout(repo, &manifest)?;
    let synced = !no_sync && env::sync(&repo.root)?;
    Ok(Forked {
        op,
        notices,
        restored,
        fork_step,
        synced,
    })
}

fn text(repo: &Repo, m: &Manifest, path: &str) -> Result<Option<Vec<u8>>> {
    match m.get(path) {
        Some(e) => Ok(Some(repo.objects.read_blob(&e.hash)?)),
        None => Ok(None),
    }
}

/// Outcome of `apply`: files written and files left with conflicts.
pub struct Applied {
    pub op: OpRecord,
    pub changed: Vec<String>,
    pub conflicts: Vec<String>,
}

/// Three-way apply of `node`'s code delta (parent → node) onto the working copy.
/// Conflicts are written as markers; nothing is refused.
pub fn apply(repo: &mut Repo, node: &str) -> Result<Applied> {
    let n = repo.node(&repo.resolve(node)?)?;
    let parent = n
        .parent
        .clone()
        .ok_or_else(|| msg(format!("{} is a root; it has no delta to apply", n.id)))?;
    let pm = repo
        .objects
        .get_manifest(&wc::manifest_of(repo, &repo.node(&parent)?.code)?)?;
    let nm = repo
        .objects
        .get_manifest(&wc::manifest_of(repo, &n.code)?)?;
    repo.objects.validate_destinations(&pm, &repo.root)?;
    repo.objects.validate_destinations(&nm, &repo.root)?;
    let saved = wc::snapshot_manifest(repo)?;
    let current = repo.objects.get_manifest(&saved)?;
    let (mut changed, mut conflicts) = (vec![], vec![]);
    let root = repo.root.clone();
    for c in pollard_objects::diff(&pm, &nm) {
        let dest = root.join(&c.path);
        if current.get(&c.path).is_none() && std::fs::symlink_metadata(&dest).is_ok() {
            conflicts.push(c.path);
            continue;
        }
        let ours = text(repo, &current, &c.path)?;
        let ours_mode = current.get(&c.path).map(|e| e.mode);
        let base_mode = pm.get(&c.path).map(|e| e.mode);
        let theirs_mode = nm.get(&c.path).map(|e| e.mode);
        let base = text(repo, &pm, &c.path)?;
        let theirs = text(repo, &nm, &c.path)?;
        if ours == theirs && ours_mode == theirs_mode {
            continue;
        }
        if ours == base && ours_mode == base_mode {
            match c.kind {
                ChangeKind::Removed => {
                    std::fs::remove_file(&dest).map_err(|e| crate::Error::Io(dest.clone(), e))?;
                }
                _ => repo.objects.restore_at(nm.get(&c.path).unwrap(), &root)?,
            }
            changed.push(c.path);
            continue;
        }
        let mode = if ours_mode == base_mode {
            theirs_mode
        } else if theirs_mode == base_mode || ours_mode == theirs_mode {
            ours_mode
        } else {
            None
        };
        // Type conflicts and symlink targets cannot be merged as file contents.
        if mode.is_none()
            || [ours_mode, base_mode, theirs_mode].contains(&Some(pollard_objects::MODE_LINK))
        {
            conflicts.push(c.path);
            continue;
        }
        if (base == theirs || ours == theirs) && ours.is_some() {
            let bytes = ours.as_ref().unwrap();
            let (hash, size) = repo.objects.put_bytes(bytes)?;
            let entry = pollard_objects::Entry {
                path: c.path.clone(),
                hash,
                size,
                mode: mode.unwrap(),
            };
            repo.objects.restore_at(&entry, &root)?;
            changed.push(c.path);
            continue;
        }
        // real conflict: three-way text merge
        let s = |b: &Option<Vec<u8>>| {
            b.as_deref()
                .map(|b| String::from_utf8(b.to_vec()).ok())
                .unwrap_or(Some(String::new()))
        };
        match (s(&base), s(&ours), s(&theirs)) {
            (Some(b), Some(o), Some(t)) if c.kind != ChangeKind::Removed => {
                let merged =
                    match linewise_merge(&b, &o, &t).map_or_else(|| diffy::merge(&b, &o, &t), Ok) {
                        Ok(m) => m,
                        Err(m) => {
                            conflicts.push(c.path.clone());
                            m
                        }
                    };
                let (hash, size) = repo.objects.put_bytes(merged.as_bytes())?;
                let entry = pollard_objects::Entry {
                    path: c.path.clone(),
                    hash,
                    size,
                    mode: mode.unwrap(),
                };
                repo.objects.restore_at(&entry, &root)?;
                changed.push(c.path);
            }
            _ => conflicts.push(c.path),
        }
    }
    let id = n.id.clone();
    let op = ops::record(repo, Some(saved), |_| Ok(id))?;
    Ok(Applied {
        op,
        changed,
        conflicts,
    })
}

/// In-place line edits (same line count on all sides) merge per line, so edits on adjacent
/// lines (`lr` vs `depth` in a config) don't conflict the way diff3 hunks would.
fn linewise_merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (b, o, t): (Vec<&str>, Vec<&str>, Vec<&str>) = (
        base.split_inclusive('\n').collect(),
        ours.split_inclusive('\n').collect(),
        theirs.split_inclusive('\n').collect(),
    );
    if b.len() != o.len() || b.len() != t.len() {
        return None;
    }
    let mut out = String::new();
    for i in 0..b.len() {
        out.push_str(match (o[i] == b[i], t[i] == b[i]) {
            (true, _) => t[i],
            (_, true) => o[i],
            _ if o[i] == t[i] => o[i],
            _ => return None,
        });
    }
    Some(out)
}

fn unified(path: &str, a: &[u8], b: &[u8], out: &mut String) {
    match (std::str::from_utf8(a), std::str::from_utf8(b)) {
        (Ok(a), Ok(b)) => {
            let p = diffy::create_patch(a, b);
            let body = p.to_string();
            let _ = writeln!(out, "--- a/{path}\n+++ b/{path}");
            // skip diffy's own ---/+++ header lines
            for l in body.lines().skip(2) {
                let _ = writeln!(out, "{l}");
            }
        }
        _ => {
            let _ = writeln!(out, "binary file {path} differs");
        }
    }
}

fn manifest_diff_text(
    repo: &Repo,
    a: &Manifest,
    b: &Manifest,
    skip: Option<&str>,
    out: &mut String,
) -> Result<()> {
    for c in pollard_objects::diff(a, b) {
        if Some(c.path.as_str()) == skip {
            continue;
        }
        let x = text(repo, a, &c.path)?.unwrap_or_default();
        let y = text(repo, b, &c.path)?.unwrap_or_default();
        unified(&c.path, &x, &y, out);
    }
    Ok(())
}

/// `pollard diff a b`: siblings use the parent-relative table (§6); others a direct diff.
pub fn diff(repo: &Repo, a: &str, b: &str, docs: bool) -> Result<String> {
    let (na, nb) = (repo.node(&repo.resolve(a)?)?, repo.node(&repo.resolve(b)?)?);
    let mut out = String::new();
    if docs {
        let m = |h: &Option<String>| -> Result<Manifest> {
            h.as_ref().map_or(Ok(Manifest::default()), |h| {
                Ok(repo.objects.get_manifest(h)?)
            })
        };
        manifest_diff_text(repo, &m(&na.docs)?, &m(&nb.docs)?, None, &mut out)?;
        if out.is_empty() {
            out.push_str("no off-tree changes\n");
        }
        return Ok(out);
    }
    if na.parent.is_some() && na.parent == nb.parent && na.id != nb.id {
        let t = siblings::build(
            repo,
            na.parent.as_deref().unwrap(),
            &siblings::Opts {
                only: Some(vec![na.id.clone(), nb.id.clone()]),
                all: true,
                ..Default::default()
            },
        )?;
        let _ = writeln!(out, "siblings of {} (each vs. the parent)", t.parent);
        out.push_str(&siblings::render(&t));
        return Ok(out);
    }
    let _ = writeln!(out, "{} → {}", na.id, nb.id);
    let cfg = delta::config_delta(
        &wc::load_config(repo, &na.config)?,
        &wc::load_config(repo, &nb.config)?,
    );
    for c in &cfg {
        let _ = writeln!(out, "config  {}  {}", c.path, fmt_change(c));
    }
    let ma = repo
        .objects
        .get_manifest(&wc::manifest_of(repo, &na.code)?)?;
    let mb = repo
        .objects
        .get_manifest(&wc::manifest_of(repo, &nb.code)?)?;
    let skip = match wc::capture_mode(repo) {
        wc::Capture::File(p) => Some(p),
        _ => None,
    };
    let code: Vec<_> = pollard_objects::diff(&ma, &mb)
        .into_iter()
        .filter(|c| Some(&c.path) != skip.as_ref())
        .collect();
    if !code.is_empty() {
        let _ = writeln!(out, "code    {}", delta::summarize_changes(&code));
    }
    let (da, db) = (
        repo.objects.get_manifest(&na.data)?,
        repo.objects.get_manifest(&nb.data)?,
    );
    let dd = pollard_objects::diff(&da, &db);
    if !dd.is_empty() {
        let _ = writeln!(out, "data    {}", delta::summarize_changes(&dd));
    }
    let ed = env::delta(repo, &na.env, &nb.env);
    if !ed.is_empty() {
        let _ = writeln!(out, "env     {}", delta::summarize_env(&ed));
    }
    let keys: std::collections::BTreeSet<String> = metrics::keys(&repo.db, &na.id)?
        .into_iter()
        .chain(metrics::keys(&repo.db, &nb.id)?)
        .collect();
    for k in keys {
        let la = metrics::series(&repo.db, &na.id, &k)?.last().copied();
        let lb = metrics::series(&repo.db, &nb.id, &k)?.last().copied();
        let f = |p: Option<(i64, f64)>| {
            p.map_or("—".to_string(), |(s, v)| {
                format!("{}@{}", siblings::fmt_num(v), siblings::fmt_step(s))
            })
        };
        let _ = writeln!(out, "metric  {k}  {}  {}", f(la), f(lb));
    }
    manifest_diff_text(repo, &ma, &mb, skip.as_deref(), &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn linewise() {
        let m = super::linewise_merge(
            "lr: 3\ndepth: 12\n",
            "lr: 3\ndepth: 24\n",
            "lr: 1\ndepth: 12\n",
        );
        assert_eq!(m.as_deref(), Some("lr: 1\ndepth: 24\n"));
        assert_eq!(super::linewise_merge("x = 1\n", "x = 3\n", "x = 2\n"), None);
    }
}
