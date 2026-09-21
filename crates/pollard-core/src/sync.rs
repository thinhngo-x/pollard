//! `push` / `pull` (§5 Remote). Owner: senior dev. Blob transfer is `pollard-remote`.
//!
//! Remote format 2 (`FORMAT` holds `2`). Each push writes one new, never rewritten segment
//! `nodes/<utc-ts>-<clone_salt>-<n>.jsonl`, so concurrent pushes cannot lose each other's
//! lines. A segment holds node lines `{node, deltas, metrics, code_manifest}` (metrics as a
//! blob hash) and pin records `{"pin": [node_id, name|null], "removed": bool}`, one per pin
//! change. Readers replay format 1's `nodes.jsonl` / `pins.json` first, then the segments in
//! name order: the last line per node id wins (`note`/`status` last-writer-wins) and pin
//! records apply in order, so only two edits of the same pin conflict.
//! The first push of a format-2 client converts a format-1 remote: `FORMAT`, plus a poison
//! line in `nodes.jsonl` that 0.1 clients fail to parse before they write anything.
//! Meta `sync:<id>` holds the hash of the line this clone last pushed or pulled per node, so
//! a push only sends local changes and a pull never clobbers unpushed local edits. The
//! `remote_segments` table lists the segments already read, with their pin records.

use std::collections::{BTreeSet, HashMap};

use pollard_remote::{Remote, Stats};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::delta::{self, Deltas};
use crate::node::{self, Node};
use crate::{OpRecord, Repo, Result, msg, ops, wc};

/// Remote format this build reads and writes.
const FORMAT: i64 = 2;
const POISON: &str = r#"{"pollard_format":2,"upgrade":"this remote was converted by pollard 0.2.0-alpha.1; upgrade pollard to push or pull"}"#;
/// `remote_segments` row holding format 1's `pins.json` (sorts before every `nodes/…`).
const LEGACY_ROW: &str = "0-legacy";

#[derive(Serialize, Deserialize)]
struct Line {
    node: Node,
    deltas: Deltas,
    /// blob hash of `[[key, step, value, ts], ...]`, or None if no metrics
    metrics: Option<String>,
    /// code manifest behind `node.code` (the `code_trees` row)
    code_manifest: Option<String>,
}

/// `(node_id, name)`.
type Pin = (String, Option<String>);
type Pins = BTreeSet<Pin>;

/// `(line hash, line)` in file order.
type HashedLines = Vec<(String, Line)>;

/// One pin change.
#[derive(Serialize, Deserialize)]
struct PinRec {
    pin: Pin,
    removed: bool,
}

fn remote_err(e: pollard_remote::Error) -> crate::Error {
    msg(e.to_string())
}

fn open(repo: &Repo) -> Result<Remote> {
    let url = repo.config.remote.as_deref().ok_or_else(|| {
        msg(format!(
            "no remote configured: set `remote = \"<path or s3://bucket/prefix>\"` in {}",
            repo.dot.join("config.toml").display()
        ))
    })?;
    let url = if url.contains("://") || url.starts_with('/') {
        url.to_string()
    } else {
        repo.root.join(url).display().to_string()
    };
    Remote::open(&url).map_err(remote_err)
}

/// The remote's format: `None` for a format-1 remote. Refuses a newer one.
fn remote_format(remote: &Remote) -> Result<Option<i64>> {
    let Some(raw) = remote.get("FORMAT").map_err(remote_err)? else {
        return Ok(None);
    };
    let v: i64 = String::from_utf8_lossy(&raw)
        .trim()
        .parse()
        .map_err(|_| msg(format!("remote {}: unreadable FORMAT file", remote.url())))?;
    if v > FORMAT {
        return Err(msg(format!(
            "remote {} uses format {v}, written by a newer pollard; this pollard ({}) reads up to format {FORMAT}. Upgrade pollard.",
            remote.url(),
            env!("CARGO_PKG_VERSION")
        )));
    }
    Ok(Some(v))
}

fn line_of(repo: &Repo, n: &Node) -> Result<(String, String)> {
    let mut st = repo.db.prepare(
        "SELECT key, step, value, ts FROM metrics WHERE node_id=?1 ORDER BY key, step, ts",
    )?;
    let rows: Vec<(String, i64, f64, String)> = st
        .query_map([&n.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let metrics = match rows.is_empty() {
        true => None,
        false => Some(repo.objects.put_bytes(&serde_json::to_vec(&rows)?)?.0),
    };
    let line = Line {
        node: n.clone(),
        deltas: delta::load(repo, &n.id)?,
        metrics,
        code_manifest: wc::manifest_of(repo, &n.code).ok(),
    };
    let json = serde_json::to_string(&line)?;
    Ok((crate::b3(json.as_bytes()), json))
}

/// Node lines (with their hash) and pin records of one remote file. Format-1 lines with
/// status `pruned` get the migration's fallback (`done` if finished, else `killed`).
fn parse_file(
    remote: &Remote,
    name: &str,
    raw: &[u8],
    legacy: bool,
) -> Result<(HashedLines, Vec<PinRec>)> {
    let (mut lines, mut pins) = (vec![], vec![]);
    for (i, text) in String::from_utf8_lossy(raw).lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let at = || format!("remote {} {name} line {}", remote.url(), i + 1);
        let mut v: Value = serde_json::from_str(text).map_err(|e| msg(format!("{}: {e}", at())))?;
        if v.get("pollard_format").is_some() {
            continue; // the poison line for 0.1 clients
        }
        if v.get("pin").is_some() {
            pins.push(serde_json::from_value(v).map_err(|e| msg(format!("{}: {e}", at())))?);
            continue;
        }
        let n = &mut v["node"];
        if legacy && n["status"] == "pruned" {
            let finished = n["finished_at"].clone();
            n["status"] = if finished.is_null() { "killed" } else { "done" }.into();
            n["pruned_at"] = if finished.is_null() {
                n["created_at"].clone()
            } else {
                finished
            };
        }
        let id = n["id"].as_str().unwrap_or("?").to_string();
        let line: Line =
            serde_json::from_value(v).map_err(|e| msg(format!("{}: node {id}: {e}", at())))?;
        // hash of the line as this build writes it, so it compares with `line_of`
        lines.push((crate::b3(serde_json::to_string(&line)?.as_bytes()), line));
    }
    Ok((lines, pins))
}

/// Format 1's `pins.json` (`{name: node_id}`) as pin records.
fn legacy_pins(remote: &Remote, stats: &mut SyncStats) -> Result<Vec<PinRec>> {
    let Some(raw) = remote.get("pins.json").map_err(remote_err)? else {
        return Ok(vec![]);
    };
    stats.blobs.bytes += raw.len() as u64;
    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(&raw)
        .map_err(|e| msg(format!("remote {} pins.json: {e}", remote.url())))?;
    Ok(map
        .into_iter()
        .map(|(name, id)| PinRec {
            pin: (id, Some(name)),
            removed: false,
        })
        .collect())
}

/// Changes whenever format 1's files do (0.1 clients append; conversion adds the poison).
fn legacy_sig(remote: &Remote) -> Result<String> {
    Ok(format!(
        "{:?} {:?}",
        remote.stat("nodes.jsonl").map_err(remote_err)?,
        remote.stat("pins.json").map_err(remote_err)?
    ))
}

fn apply_pin(state: &mut Pins, r: &PinRec) {
    if r.removed {
        state.remove(&r.pin);
        return;
    }
    if r.pin.1.is_some() {
        state.retain(|p| p.1 != r.pin.1); // a name points at one node
    }
    state.insert(r.pin.clone());
}

/// Records turning pin set `from` into `to`: removals first, then additions.
fn pin_changes(from: &Pins, to: &Pins) -> Vec<PinRec> {
    let rm = from.difference(to).map(|p| PinRec {
        pin: p.clone(),
        removed: true,
    });
    let add = to.difference(from).map(|p| PinRec {
        pin: p.clone(),
        removed: false,
    });
    rm.chain(add).collect()
}

/// The remote's pins as far as this clone has read (or written) them.
fn known_pins(repo: &Repo) -> Result<Pins> {
    let mut st = repo
        .db
        .prepare("SELECT pins FROM remote_segments ORDER BY name")?;
    let mut state = Pins::new();
    for row in st.query_map([], |r| r.get::<_, String>(0))? {
        for r in serde_json::from_str::<Vec<PinRec>>(&row?)? {
            apply_pin(&mut state, &r);
        }
    }
    Ok(state)
}

fn local_pins(repo: &Repo) -> Result<Pins> {
    let mut st = repo.db.prepare("SELECT node_id, name FROM pins")?;
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

fn remember_segment(repo: &Repo, name: &str, pins: &[PinRec]) -> Result<()> {
    repo.db.execute(
        "INSERT OR REPLACE INTO remote_segments(name, pins) VALUES(?1, ?2)",
        params![name, serde_json::to_string(pins)?],
    )?;
    Ok(())
}

/// What `push`/`pull` did.
#[derive(Debug, Default)]
pub struct SyncStats {
    pub nodes: usize,
    pub pins: bool,
    pub blobs: Stats,
}

/// Upload missing blobs, then write one segment with the lines of nodes changed locally
/// since the last sync and the local pin changes. A push with nothing new writes nothing.
/// The first push to a format-1 remote converts it and sends every node line.
pub fn push(repo: &Repo) -> Result<SyncStats> {
    let remote = open(repo)?;
    let _lock = repo.lock()?;
    let converting = remote_format(&remote)?.is_none();
    let mut stats = SyncStats::default();
    let mut seg = String::new();
    let mut markers = vec![];
    for n in node::all(&repo.db)? {
        let (h, json) = line_of(repo, &n)?;
        let key = format!("sync:{}", n.id);
        if converting || repo.meta(&key)?.as_deref() != Some(h.as_str()) {
            seg.push_str(&json);
            seg.push('\n');
            stats.nodes += 1;
        }
        markers.push((key, h));
    }
    // Converting: this push re-sends every node, so format 1's files hold nothing new for
    // this clone if it has all their nodes and pins; then count them as read, and the next
    // pull needs no full replay.
    let mut legacy = None;
    if converting {
        let raw = remote
            .get("nodes.jsonl")
            .map_err(remote_err)?
            .unwrap_or_default();
        let base = legacy_pins(&remote, &mut SyncStats::default())?;
        let mine = local_pins(repo)?;
        let mut known = base.iter().all(|r| mine.contains(&r.pin));
        for (_, l) in parse_file(&remote, "nodes.jsonl", &raw, true)?.0 {
            known &= node::get(&repo.db, &l.node.id)?.is_some();
        }
        if known {
            remember_segment(repo, LEGACY_ROW, &base)?;
        }
        legacy = Some((raw, known));
    }
    let pins = pin_changes(&known_pins(repo)?, &local_pins(repo)?);
    for r in &pins {
        seg.push_str(&serde_json::to_string(r)?);
        seg.push('\n');
    }
    stats.pins = !pins.is_empty();
    // blobs after the lines (`line_of` stores metric blobs), so a segment never names a
    // blob the remote lacks
    stats.blobs = remote.push_blobs(&repo.objects).map_err(remote_err)?;
    let mut written = None;
    if !seg.is_empty() {
        let n: u64 = repo
            .meta("sync:seq")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
            + 1;
        let ts: String = crate::now()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let salt = repo.meta("salt")?.unwrap_or_default();
        let name = format!("nodes/{ts}-{salt}-{n}.jsonl");
        stats.blobs.bytes += seg.len() as u64;
        remote.put(&name, seg.into_bytes()).map_err(remote_err)?;
        repo.set_meta("sync:seq", &n.to_string())?;
        written = Some(name);
    }
    if let Some((mut raw, known)) = legacy {
        if !raw.is_empty() && !raw.ends_with(b"\n") {
            raw.push(b'\n');
        }
        raw.extend_from_slice(POISON.as_bytes());
        raw.push(b'\n');
        remote.put("nodes.jsonl", raw).map_err(remote_err)?;
        remote
            .put("FORMAT", FORMAT.to_string().into_bytes())
            .map_err(remote_err)?;
        if known {
            repo.set_meta("sync:legacy", &legacy_sig(&remote)?)?;
        }
        eprintln!(
            "pollard: converted remote {} to format {FORMAT}\n  pollard 0.1.0-alpha.1 can no longer push to or pull from it: everyone sharing it must upgrade\n  this push re-sends all {} node records once (no weights)",
            remote.url(),
            stats.nodes
        );
    }
    for (key, hash) in markers {
        repo.set_meta(&key, &hash)?;
    }
    if let Some(name) = written {
        remember_segment(repo, &name, &pins)?;
    }
    Ok(stats)
}

/// Download missing blobs and the segments not read yet, and merge their nodes and pins
/// (one op, so `undo` reverts it). A remote node replaces the local one only if the local one
/// is unchanged since the last sync; local pin changes not pushed yet are kept on top of the
/// remote's. Refuses, before changing anything, a remote node whose id exists locally as a
/// different node (different recipe and creation time), naming the id.
pub fn pull(repo: &mut Repo) -> Result<(OpRecord, SyncStats)> {
    let remote = open(repo)?;
    remote_format(&remote)?;
    let mut stats = SyncStats::default();
    // Format-1 files are re-read when they change (0.1 clients append; conversion adds the
    // poison line); segments when new. A segment that sorts before one already read (clock
    // skew, a slow concurrent push) or a changed format-1 file means a full replay, so every
    // clone ends up with the same last line per node.
    let legacy_sig = legacy_sig(&remote)?;
    let legacy_changed = repo.meta("sync:legacy")?.as_deref() != Some(legacy_sig.as_str());
    let read: BTreeSet<String> = {
        let mut st = repo
            .db
            .prepare("SELECT name FROM remote_segments WHERE name != ?1")?;
        st.query_map([LEGACY_ROW], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut segs: Vec<String> = remote
        .list("nodes")
        .map_err(remote_err)?
        .into_iter()
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    segs.sort();
    let new: Vec<&String> = segs.iter().filter(|s| !read.contains(*s)).collect();
    let full = legacy_changed || new.first().zip(read.last()).is_some_and(|(n, r)| *n < r);
    let mut lines: Vec<(String, Line)> = vec![];
    let mut legacy_base: Option<Vec<PinRec>> = None;
    if legacy_changed {
        if let Some(raw) = remote.get("nodes.jsonl").map_err(remote_err)? {
            stats.blobs.bytes += raw.len() as u64;
            lines.extend(parse_file(&remote, "nodes.jsonl", &raw, true)?.0);
        }
        legacy_base = Some(legacy_pins(&remote, &mut stats)?);
    }
    let mut seg_pins = vec![];
    for name in if full { segs.iter().collect() } else { new } {
        let raw = remote.get(name).map_err(remote_err)?.unwrap_or_default();
        stats.blobs.bytes += raw.len() as u64;
        let (l, p) = parse_file(&remote, name, &raw, false)?;
        lines.extend(l);
        seg_pins.push((name.clone(), p));
    }
    // last line per id, in first-seen order
    let mut order: Vec<String> = vec![];
    let mut latest: HashMap<String, (String, Line)> = HashMap::new();
    for (h, l) in lines {
        let id = l.node.id.clone();
        if latest.insert(id.clone(), (h, l)).is_none() {
            order.push(id);
        }
    }
    let lines: Vec<(String, String, Line)> = order
        .into_iter()
        .map(|id| {
            let (h, l) = latest.remove(&id).expect("inserted above");
            (id, h, l)
        })
        .collect();
    for (id, _, l) in &lines {
        if let Some(local) = node::get(&repo.db, id)?
            && local.recipe_hash != l.node.recipe_hash
            && local.created_at != l.node.created_at
        {
            return Err(msg(format!(
                "pull: node id {id} exists here with a different recipe (local {}, remote {}); refusing",
                &local.recipe_hash[..12.min(local.recipe_hash.len())],
                &l.node.recipe_hash[..12.min(l.node.recipe_hash.len())]
            )));
        }
    }
    let rec = ops::record(repo, None, |repo| {
        let blobs = remote.pull_blobs(&repo.objects).map_err(remote_err)?;
        stats.blobs.files += blobs.files;
        stats.blobs.bytes += blobs.bytes;
        for (id, h, l) in &lines {
            let key = format!("sync:{id}");
            let (replace, same) = match node::get(&repo.db, id)? {
                None => (true, false),
                Some(local) => {
                    let (lh, _) = line_of(repo, &local)?;
                    (
                        lh != *h && repo.meta(&key)?.as_deref() == Some(lh.as_str()),
                        lh == *h,
                    )
                }
            };
            if replace {
                apply_line(repo, l)?;
                stats.nodes += 1;
            }
            if replace || same {
                repo.set_meta(&key, h)?;
            }
        }
        let before = known_pins(repo)?;
        if full {
            repo.db
                .execute("DELETE FROM remote_segments WHERE name != ?1", [LEGACY_ROW])?;
        }
        if let Some(p) = &legacy_base {
            remember_segment(repo, LEGACY_ROW, p)?;
        }
        for (name, p) in &seg_pins {
            remember_segment(repo, name, p)?;
        }
        let mine = local_pins(repo)?;
        let mut merged = known_pins(repo)?;
        for r in pin_changes(&before, &mine) {
            apply_pin(&mut merged, &r);
        }
        if merged != mine {
            repo.db.execute("DELETE FROM pins", [])?;
            for (id, name) in &merged {
                repo.db.execute(
                    "INSERT INTO pins(node_id, name) VALUES(?1, ?2)",
                    params![id, name],
                )?;
            }
            stats.pins = true;
        }
        repo.set_meta("sync:legacy", &legacy_sig)?;
        Ok(repo.head()?.unwrap_or_default())
    })?;
    Ok((rec, stats))
}

fn apply_line(repo: &Repo, l: &Line) -> Result<()> {
    let id = &l.node.id;
    repo.db.execute("DELETE FROM nodes WHERE id=?1", [id])?;
    l.node.insert(&repo.db)?;
    delta::store(repo, id, &l.deltas)?;
    if let Some(m) = &l.code_manifest {
        repo.db.execute(
            "INSERT OR IGNORE INTO code_trees(git_tree, manifest_hash) VALUES(?1,?2)",
            [&l.node.code, m],
        )?;
    }
    repo.db
        .execute("DELETE FROM metrics WHERE node_id=?1", [id])?;
    if let Some(h) = &l.metrics {
        let rows: Vec<(String, i64, f64, String)> =
            serde_json::from_slice(&repo.objects.read_blob(h)?)?;
        for (k, s, v, ts) in rows {
            repo.db.execute(
                "INSERT INTO metrics(node_id,key,step,value,ts) VALUES(?1,?2,?3,?4,?5)",
                params![id, k, s, v, ts],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Status;

    fn clone_at(dir: &std::path::Path, remote: &std::path::Path) -> Repo {
        std::fs::create_dir_all(dir).unwrap();
        let mut r = Repo::init(dir).unwrap();
        r.config.remote = Some(remote.display().to_string());
        r
    }

    fn mk(repo: &Repo, id: &str, parent: Option<&str>, recipe: &str) {
        let n = Node {
            id: id.into(),
            parent: parent.map(Into::into),
            code: "c".into(),
            config: "c".into(),
            data: "d".into(),
            env: "e".into(),
            recipe_hash: recipe.into(),
            weights: None,
            docs: None,
            fork_step: None,
            status: Status::Done,
            created_at: crate::now(),
            finished_at: None,
            command: "x".into(),
            note: Some("orig".into()),
            note_auto: false,
            lock_ok: None,
            depth: 0,
            sweep: None,
            pruned_at: None,
        };
        n.insert(&repo.db).unwrap();
        delta::store(repo, id, &Deltas::default()).unwrap();
    }

    fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
        let mut v: Vec<_> = ignore::WalkBuilder::new(dir)
            .standard_filters(false)
            .build()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| (e.path().display().to_string(), e.metadata().unwrap().len()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn two_clones_push_pull() {
        let t = tempfile::tempdir().unwrap();
        let remote = t.path().join("remote");
        let mut a = clone_at(&t.path().join("a"), &remote);
        let mut b = clone_at(&t.path().join("b"), &remote);
        mk(&a, "p", None, "r0");
        crate::metrics::ingest_line(&a.db, "p", r#"{"step":1,"loss":2.5}"#).unwrap();
        a.cmdline = "pin".into();
        crate::cmd::pin(&mut a, "p", "best").unwrap();
        let s = push(&a).unwrap();
        assert_eq!((s.nodes, s.pins), (1, true));

        let (_, s) = pull(&mut b).unwrap();
        assert_eq!(s.nodes, 1);
        assert_eq!(b.node("p").unwrap().note.as_deref(), Some("orig"));
        assert_eq!(
            crate::metrics::series(&b.db, "p", "loss").unwrap(),
            [(1, 2.5)]
        );
        assert!(
            local_pins(&b)
                .unwrap()
                .contains(&("p".into(), Some("best".into())))
        );

        mk(&a, "c1", Some("p"), "r1");
        mk(&b, "c2", Some("p"), "r2");
        push(&a).unwrap();
        push(&b).unwrap();
        pull(&mut a).unwrap();
        pull(&mut b).unwrap();
        for r in [&a, &b] {
            for id in ["p", "c1", "c2"] {
                r.node(id).unwrap();
            }
        }
        // second push: nothing changes on the remote
        let before = listing(&remote);
        assert_eq!(push(&a).unwrap().blobs, Stats::default());
        push(&b).unwrap();
        assert_eq!(listing(&remote), before);

        // note: last writer wins; a clone that did not edit does not clobber it
        b.db.execute("UPDATE nodes SET note='renamed' WHERE id='p'", [])
            .unwrap();
        push(&b).unwrap();
        push(&a).unwrap();
        pull(&mut a).unwrap();
        assert_eq!(a.node("p").unwrap().note.as_deref(), Some("renamed"));

        // collision: same id, different recipe and creation time
        let mut c = clone_at(&t.path().join("c"), &remote);
        mk(&c, "c1", None, "other");
        let e = pull(&mut c).err().unwrap().to_string();
        assert!(e.contains("c1") && e.contains("different recipe"), "{e}");

        a.config.remote = None;
        assert!(push(&a).unwrap_err().to_string().contains("remote"));
    }
}
