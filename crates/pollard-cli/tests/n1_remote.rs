//! Phase 1 (format 2): remote format 2 and concurrent pushes (BACKLOG F9, F10, and the
//! remote half of F7). Local-path remotes; MinIO is phase 2 (B12).
mod common;
use common::*;
use std::path::Path;
use std::process::Stdio;

const POISON: &str = r#"{"pollard_format":2,"upgrade":"this remote was converted by pollard 0.2.0-alpha.1; upgrade pollard to push or pull"}"#;
const NEWER_REMOTE_TAIL: &str = " uses format 3, written by a newer pollard; this pollard (0.2.0-alpha.1) reads up to format 2. Upgrade pollard.";

/// D19 is OPEN (owner). Default under test: lossless pins, one remote record per pin change,
/// so concurrent pin edits from two clones both survive. If the owner picks D-29
/// last-writer-wins on the whole pin map instead, flip this to `false`: the concurrent-pins
/// test then asserts "exactly A's or B's map, never a mix".
const D19_LOSSLESS_PINS: bool = true;

/// Every file under `dir` with its bytes: "byte-identical remote" checks.
fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn go(base: &Path, d: &Path, v: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                go(base, &p, v)
            } else {
                v.push((
                    p.strip_prefix(base).unwrap().display().to_string(),
                    std::fs::read(&p).unwrap(),
                ))
            }
        }
    }
    let mut v = vec![];
    go(dir, dir, &mut v);
    v.sort();
    v
}

fn listing_diff(a: &[(String, Vec<u8>)], b: &[(String, Vec<u8>)]) -> String {
    let ma: std::collections::BTreeMap<_, _> = a.iter().cloned().collect();
    let mb: std::collections::BTreeMap<_, _> = b.iter().cloned().collect();
    let mut out = String::new();
    for (k, v) in &ma {
        match mb.get(k) {
            None => out += &format!("removed {k}\n"),
            Some(w) if w != v => out += &format!("changed {k}\n"),
            _ => {}
        }
    }
    for k in mb.keys() {
        if !ma.contains_key(k) {
            out += &format!("added {k}\n");
        }
    }
    out
}

/// Names under `<remote>/nodes/`, sorted.
fn segments(remote: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(remote.join("nodes")) else {
        return vec![];
    };
    let mut v: Vec<String> = rd
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `<utc-ts>-<clone_salt>-<n>.jsonl`
fn assert_segment_name(name: &str) {
    let stem = name
        .strip_suffix(".jsonl")
        .unwrap_or_else(|| panic!("segment {name} is not .jsonl"));
    let parts: Vec<&str> = stem.rsplitn(3, '-').collect();
    assert!(
        parts.len() == 3
            && parts[0].chars().all(|c| c.is_ascii_digit())
            && !parts[0].is_empty()
            && !parts[1].is_empty()
            && !parts[2].is_empty(),
        "segment name {name} is not <utc-ts>-<clone_salt>-<n>.jsonl"
    );
}

fn ids_in_segment(remote: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(remote.join("nodes").join(name)).unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value =
                serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}: {e}: {l}"));
            v.get("node")?.get("id")?.as_str().map(String::from)
        })
        .collect()
}

fn fixture_statuses(r: &Repo) -> String {
    r.sql("SELECT id, status, coalesce(pruned_at,'') FROM nodes ORDER BY id;")
}

fn pin_map(r: &Repo) -> String {
    r.sql("SELECT coalesce(name,'<unnamed>'), node_id FROM pins ORDER BY 1, 2;")
}

fn fresh_remote() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("remote");
    std::fs::create_dir_all(&p).unwrap();
    (d, p)
}

/// Spawn `pollard push` in both clones before waiting on either.
fn push_together(a: &Repo, b: &Repo) -> (Out, Out) {
    let spawn = |r: &Repo| {
        let mut c = r.cmd_in(&r.root, &bin(), &["push"]);
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        c.spawn().unwrap()
    };
    let (ca, cb) = (spawn(a), spawn(b));
    let wait = |c: std::process::Child| {
        let o = c.wait_with_output().unwrap();
        Out {
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
            code: o.status.code(),
            elapsed: Default::default(),
            cmdline: "pollard push (concurrent)".into(),
        }
    };
    (wait(ca), wait(cb))
}

// ------------------------------------------------------------------------------------------
// F9. Remote format 2
// ------------------------------------------------------------------------------------------

/// F9: a migrated clone pulls the alpha.1 remote, converts nothing, keeps the F2 statuses.
#[test]
fn f9_migrated_clone_pulls_legacy_remote_without_converting() {
    let f = Fixture::new();
    let r = &f.repo;
    r.ok(&["tree"]);
    let migrated = fixture_statuses(r);
    let before = listing(&f.remote);
    let o = r.ok(&["pull"]);
    let d = listing_diff(&before, &listing(&f.remote));
    assert!(d.is_empty(), "pull changed the alpha.1 remote:\n{d}\n{o}");
    assert_eq!(
        fixture_statuses(r),
        migrated,
        "pull changed the recovered statuses\n{o}"
    );
    let t = r.ok(&["tree", "--all"]).stdout;
    for (role, id) in f.roles() {
        assert!(
            t.contains(&id),
            "{role} missing from tree --all after pull:\n{t}"
        );
    }
}

/// F9: legacy `"status":"pruned"` lines get the F2 fallback in a fresh 0.2 clone.
#[test]
fn f9_fresh_clone_of_legacy_remote_normalizes_pruned_lines() {
    let f = Fixture::new();
    let before = listing(&f.remote);
    let c = init_with_remote(&f.remote);
    let o = c.ok(&["pull"]);
    assert!(
        listing_diff(&before, &listing(&f.remote)).is_empty(),
        "pull wrote to the remote\n{o}"
    );
    assert_eq!(
        c.sql("SELECT count(*) FROM nodes WHERE status='pruned';")
            .trim(),
        "0"
    );
    for role in ["mid", "best", "crashed", "stopped", "kept", "ghost", "redo"] {
        let id = f.id(role);
        let finished = golden(&format!("show_{role}.txt"))
            .lines()
            .find_map(|l| l.strip_prefix("finished").map(|s| s.trim().to_string()))
            .unwrap_or_default();
        let (s, p) = c.status_of(&id);
        let want = if finished.is_empty() {
            "killed"
        } else {
            "done"
        };
        assert_eq!(s, want, "{role}: fallback status");
        assert!(!p.is_empty(), "{role}: pruned_at not set");
        if !finished.is_empty() {
            assert_eq!(p, finished, "{role}: fallback pruned_at = finished_at");
        }
    }
    assert_eq!(c.status_of(&f.id("late")), ("done".into(), String::new()));
}

/// F9: the first push converts: FORMAT=2, one new segment with every node line once, poison
/// line appended to nodes.jsonl, conversion notice. Second push writes nothing. A later push
/// adds a segment and never rewrites one.
#[test]
fn f9_first_push_converts_remote() {
    let f = Fixture::new();
    let r = &f.repo;
    r.ok(&["tree"]);
    let legacy = std::fs::read(f.remote.join("nodes.jsonl")).unwrap();
    let o = r.ok(&["push"]);
    assert_eq!(
        std::fs::read_to_string(f.remote.join("FORMAT"))
            .expect("no FORMAT")
            .trim(),
        "2",
        "{o}"
    );
    let segs = segments(&f.remote);
    assert_eq!(
        segs.len(),
        1,
        "first push should write one segment: {segs:?}\n{o}"
    );
    assert_segment_name(&segs[0]);
    let mut ids = ids_in_segment(&f.remote, &segs[0]);
    ids.sort();
    let mut all: Vec<String> = f.roles().into_iter().map(|x| x.1).collect();
    all.sort();
    assert_eq!(
        ids, all,
        "first push must re-send every node line exactly once"
    );

    let now = std::fs::read(f.remote.join("nodes.jsonl")).unwrap();
    assert!(
        now.starts_with(&legacy),
        "legacy nodes.jsonl rewritten, not appended"
    );
    let tail = String::from_utf8_lossy(&now[legacy.len()..]);
    assert_eq!(tail.trim(), POISON, "poison line");

    let lines: Vec<&str> = o.stderr.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("pollard: converted remote ") && l.ends_with(" to format 2"))
        .unwrap_or_else(|| panic!("no conversion notice:\n{o}"));
    assert_eq!(
        lines.get(at + 1).copied(),
        Some(
            "  pollard 0.1.0-alpha.1 can no longer push to or pull from it: everyone sharing it must upgrade"
        ),
        "{o}"
    );
    assert_eq!(
        lines.get(at + 2).copied(),
        Some("  this push re-sends all 10 node records once (no weights)"),
        "{o}"
    );
    assert!(
        !o.stdout.contains("converted remote"),
        "notice must be on stderr only\n{o}"
    );

    // Second push, nothing new: no file written, 0 bytes.
    let before = listing(&f.remote);
    let o2 = r.ok(&["push"]);
    assert!(
        listing_diff(&before, &listing(&f.remote)).is_empty(),
        "second push wrote:\n{o2}"
    );
    assert!(
        o2.stdout.contains(" 0 bytes"),
        "second push should report 0 bytes:\n{o2}"
    );
    assert!(!o2.stderr.contains("converted remote"), "{o2}");

    // A new node: a new segment; the first is untouched.
    let first = std::fs::read(f.remote.join("nodes").join(&segs[0])).unwrap();
    r.write("config.yaml", "lr: 77\n");
    let n = r.run(&["-m", "after conversion"], "true");
    r.ok(&["push"]);
    let segs2 = segments(&f.remote);
    assert_eq!(segs2.len(), 2, "{segs2:?}");
    assert_eq!(
        std::fs::read(f.remote.join("nodes").join(&segs[0])).unwrap(),
        first,
        "segment rewritten"
    );
    let new_seg = segs2.iter().find(|s| **s != segs[0]).unwrap();
    assert_segment_name(new_seg);
    assert_eq!(
        ids_in_segment(&f.remote, new_seg),
        vec![n],
        "second segment should hold only the new node"
    );
}

/// F9: a fresh 0.2 clone pulls the converted remote: every node, status, pruned_at, pin;
/// the poison line is skipped silently.
#[test]
fn f9_fresh_clone_pulls_converted_remote() {
    let f = Fixture::new();
    f.repo.ok(&["tree"]);
    f.repo.ok(&["push"]);
    let c = init_with_remote(&f.remote);
    let o = c.ok(&["pull"]);
    assert!(
        !o.stderr.contains("pollard_format") && !o.stderr.to_lowercase().contains("line"),
        "poison line not skipped silently:\n{o}"
    );
    assert_eq!(
        fixture_statuses(&c),
        fixture_statuses(&f.repo),
        "statuses / pruned_at differ"
    );
    assert_eq!(pin_map(&c), pin_map(&f.repo), "pins differ");
    assert_eq!(
        first_id(&c.ok(&["show", "paper"]).stdout),
        Some(f.id("best"))
    );
}

/// F9: pruned_at syncs between two 0.2 clones; Y's tree matches X's.
#[test]
fn f9_pruned_at_syncs() {
    let (_d, remote) = fresh_remote();
    let x = init_with_remote(&remote);
    let a = x.run(&["-m", "a"], "true");
    x.write("config.yaml", "lr: 1\n");
    let b = x.run_out(&["-m", "b"], "exit 2").last_line();
    assert!(is_node_id(&b));
    x.ok(&["push"]);
    let y = init_with_remote(&remote);
    y.ok(&["pull"]);
    x.ok(&["prune", &a]);
    x.ok(&["push"]);
    y.ok(&["pull"]);
    let (sx, px) = x.status_of(&a);
    assert_eq!((sx.as_str(), px.is_empty()), ("done", false));
    assert_eq!(y.status_of(&a), (sx, px.clone()), "pruned_at did not sync");
    let s = y.ok(&["show", &a]).stdout;
    let words: Vec<Vec<&str>> = s.lines().map(|l| l.split_whitespace().collect()).collect();
    assert!(words.contains(&vec!["status", "done"]), "{s}");
    assert!(words.contains(&vec!["pruned", px.as_str()]), "{s}");
    let (sb, pb) = y.status_of(&b);
    assert_eq!(sb, "failed", "b's outcome lost in sync");
    assert!(!pb.is_empty(), "b's pruned_at did not sync");
    for args in [vec!["tree"], vec!["tree", "--all"]] {
        assert_eq!(
            strip_at(&y.ok(&args).stdout),
            strip_at(&x.ok(&args).stdout),
            "{args:?} differs between clones"
        );
    }
}

/// F9 (+ PLAN N1): named and unnamed pins round-trip push → pull between two 0.2 clones.
/// The unnamed pin is stored directly (its CLI is B9).
#[test]
fn f9_pins_roundtrip_named_and_unnamed() {
    let (_d, remote) = fresh_remote();
    let x = init_with_remote(&remote);
    let a = x.run(&["-m", "a"], "true");
    x.write("config.yaml", "lr: 1\n");
    let b = x.run(&["-m", "b"], "true");
    x.ok(&["pin", &a, "p1"]);
    x.sql(&format!(
        "INSERT INTO pins(node_id, name) VALUES('{b}', NULL);"
    ));
    x.ok(&["push"]);
    let y = init_with_remote(&remote);
    y.ok(&["pull"]);
    assert_eq!(first_id(&y.ok(&["show", "p1"]).stdout), Some(a.clone()));
    assert_eq!(
        y.sql("SELECT node_id FROM pins WHERE name IS NULL;").trim(),
        b,
        "unnamed pin did not round-trip"
    );
    y.ok(&["unpin", "p1"]);
    y.ok(&["push"]);
    x.ok(&["pull"]);
    x.fail(&["show", "p1"]);
    assert_eq!(pin_map(&x), pin_map(&y));
}

/// F9: legacy pins.json is read only while no segment carries pins.
#[test]
fn f9_legacy_pins_json_only_until_segments_carry_pins() {
    let f = Fixture::new();
    let r = &f.repo;
    r.ok(&["tree"]);
    r.ok(&["push"]);
    assert!(f.remote.join("FORMAT").is_file(), "remote not converted");
    let z1 = init_with_remote(&f.remote);
    z1.ok(&["pull"]);
    for (pin, role) in [("paper", "best"), ("best-v1", "best"), ("root-pin", "root")] {
        assert_eq!(
            first_id(&z1.ok(&["show", pin]).stdout),
            Some(f.id(role)),
            "{pin}"
        );
    }
    r.ok(&["unpin", "root-pin"]);
    r.ok(&["push"]);
    let z2 = init_with_remote(&f.remote);
    z2.ok(&["pull"]);
    z2.fail(&["show", "root-pin"]);
    assert_eq!(
        first_id(&z2.ok(&["show", "paper"]).stdout),
        Some(f.id("best"))
    );
    z1.ok(&["pull"]);
    z1.fail(&["show", "root-pin"]);
}

/// F9: a second pull with nothing new downloads 0 bytes (legacy-converted and fresh remotes).
#[test]
fn f9_second_pull_zero_bytes() {
    let f = Fixture::new();
    f.repo.ok(&["tree"]);
    f.repo.ok(&["push"]);
    assert!(f.remote.join("FORMAT").is_file(), "remote not converted");
    let c = init_with_remote(&f.remote);
    c.ok(&["pull"]);
    let o = c.ok(&["pull"]);
    assert!(o.stdout.contains("pulled 0 nodes, 0 bytes"), "{o}");
    let o = f.repo.ok(&["pull"]);
    assert!(o.stdout.contains(" 0 bytes"), "{o}");
}

/// F9: FORMAT 3 refused by push and pull; nothing written locally or remotely.
#[test]
fn f9_newer_remote_format_refused() {
    let (_d, remote) = fresh_remote();
    let x = init_with_remote(&remote);
    x.run(&["-m", "a"], "true");
    x.ok(&["push"]);
    std::fs::write(remote.join("FORMAT"), "3\n").unwrap();
    x.write("config.yaml", "lr: 1\n");
    x.run(&["-m", "b"], "true");
    let local = x.dot_state();
    let rem = listing(&remote);
    for cmd in ["push", "pull"] {
        let o = x.fail(&[cmd]);
        let err = o.stderr.trim_end();
        assert!(
            err.starts_with("error: remote ")
                && err.ends_with(NEWER_REMOTE_TAIL)
                && err.lines().count() == 1,
            "{cmd} stderr should be exactly `error: remote <url>{NEWER_REMOTE_TAIL}`:\n{o}"
        );
        assert!(
            listing_diff(&rem, &listing(&remote)).is_empty(),
            "{cmd} wrote to the remote"
        );
        let d = content_diff(&local, &x.dot_state());
        assert!(d.is_empty(), "{cmd} wrote locally:\n{d}");
    }
}

/// F9: two 0.2 clones doing their first push to the same alpha.1 remote at the same time.
#[test]
fn f9_concurrent_first_push_to_alpha1_remote() {
    let f = Fixture::new();
    let a = &f.repo;
    a.ok(&["tree"]);
    a.write("config.yaml", "lr: 42\n");
    let na = a.run(&["-m", "from A"], "true");
    let b = init_with_remote(&f.remote);
    b.ok(&["pull"]);
    b.write("config.yaml", "lr: 43\n");
    let nb = b.run(&["-m", "from B"], "true");
    let (oa, ob) = push_together(a, &b);
    assert!(oa.ok(), "A's push failed:\n{oa}");
    assert!(ob.ok(), "B's push failed:\n{ob}");
    assert_eq!(
        std::fs::read_to_string(f.remote.join("FORMAT"))
            .unwrap()
            .trim(),
        "2"
    );
    let poison = std::fs::read_to_string(f.remote.join("nodes.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| l.trim() == POISON)
        .count();
    assert!((1..=2).contains(&poison), "{poison} poison lines");
    let c = init_with_remote(&f.remote);
    c.ok(&["pull"]);
    let ids = node_ids(&c);
    for id in f.roles().into_iter().map(|x| x.1).chain([na, nb]) {
        assert!(ids.contains(&id), "{id} missing in a fresh clone: {ids:?}");
    }
    assert_eq!(ids.len(), 12);
}

/// Risk probe, beyond BACKLOG: a fresh 0.2 clone of an alpha.1 remote only has *guessed*
/// statuses (fallback) for pruned nodes. If its first push re-sends those lines after the
/// migrated clone's push, do the guesses override the recovered statuses for everyone?
#[test]
#[ignore = "risk probe, not a BACKLOG criterion; run with --ignored"]
fn probe_guessed_statuses_do_not_override_recovered_ones() {
    let f = Fixture::new();
    let a = &f.repo;
    let b = init_with_remote(&f.remote);
    b.ok(&["pull"]); // legacy lines → fallback: crashed reads `done`
    a.ok(&["tree"]);
    a.ok(&["push"]); // recovered: crashed `failed`
    b.write("config.yaml", "lr: 43\n");
    b.run(&["-m", "from B"], "true");
    b.ok(&["push"]);
    let c = init_with_remote(&f.remote);
    c.ok(&["pull"]);
    assert_eq!(c.status_of(&f.id("crashed")).0, "failed");
    assert_eq!(c.status_of(&f.id("stopped")).0, "killed");
}

/// F7 (remote half): an unknown status in a remote line is an error naming node and value.
#[test]
fn f7_bogus_status_in_remote_line_is_an_error() {
    let (_d, remote) = fresh_remote();
    let x = init_with_remote(&remote);
    let a = x.run(&["-m", "a"], "true");
    x.ok(&["push"]);
    let mut edited = 0;
    for s in segments(&remote) {
        let p = remote.join("nodes").join(&s);
        let t = std::fs::read_to_string(&p).unwrap();
        let t2 = t
            .replace("\"status\":\"done\"", "\"status\":\"bogus\"")
            .replace("\"status\": \"done\"", "\"status\": \"bogus\"");
        if t2 != t {
            edited += 1;
            std::fs::write(&p, t2).unwrap();
        }
    }
    assert!(
        edited > 0,
        "no segment with a \"status\":\"done\" line under {}/nodes",
        remote.display()
    );
    let y = init_with_remote(&remote);
    let o = y.fail(&["pull"]);
    assert!(o.stderr.contains(&a) && o.stderr.contains("bogus"), "{o}");
}

// ------------------------------------------------------------------------------------------
// F10. Concurrent pushes lose nothing
// ------------------------------------------------------------------------------------------

struct Pair {
    _d: tempfile::TempDir,
    remote: std::path::PathBuf,
    a: Repo,
    b: Repo,
    base: String,
}

fn pair() -> Pair {
    let (d, remote) = fresh_remote();
    let a = init_with_remote(&remote);
    let base = a.run(&["-m", "base"], "true");
    a.ok(&["push"]);
    let b = init_with_remote(&remote);
    b.ok(&["pull"]);
    b.ok(&["fork", &base, "--no-sync"]);
    Pair {
        _d: d,
        remote,
        a,
        b,
        base,
    }
}

fn assert_all_three_agree(p: &Pair, expect: &[String]) -> Repo {
    for (name, r) in [("A", &p.a), ("B", &p.b)] {
        let o = r.ok(&["pull"]);
        let ids = node_ids(r);
        for id in expect {
            assert!(ids.contains(id), "{name} lost {id} after pull\n{o}");
        }
    }
    let c = init_with_remote(&p.remote);
    c.ok(&["pull"]);
    let ids = node_ids(&c);
    let mut want = expect.to_vec();
    want.sort();
    assert_eq!(ids, want, "fresh clone C: nodes lost or extra");
    let tc = strip_at(&c.ok(&["tree", "--all"]).stdout);
    assert_eq!(
        strip_at(&p.a.ok(&["tree", "--all"]).stdout),
        tc,
        "A vs C tree --all"
    );
    assert_eq!(
        strip_at(&p.b.ok(&["tree", "--all"]).stdout),
        tc,
        "B vs C tree --all"
    );
    c
}

/// F10: 2 clones × 20 rounds of simultaneous pushes; A, B and a fresh C see base + 40.
#[test]
fn f10_concurrent_pushes_lose_no_node() {
    let p = pair();
    let mut all = vec![p.base.clone()];
    for i in 0..20 {
        p.a.write("config.yaml", format!("lr: a{i}\n"));
        all.push(p.a.run(&["-m", &format!("a{i}")], "true"));
        p.b.write("config.yaml", format!("lr: b{i}\n"));
        all.push(p.b.run(&["-m", &format!("b{i}")], "true"));
        let (oa, ob) = push_together(&p.a, &p.b);
        assert!(oa.ok(), "round {i}: A push failed\n{oa}");
        assert!(ob.ok(), "round {i}: B push failed\n{ob}");
    }
    assert_eq!(all.len(), 41);
    assert_all_three_agree(&p, &all);
}

/// F10: same with a checkpoint per node; weights of a sample of 5 nodes restore
/// byte-identically in C via `fork --step`.
#[test]
fn f10_concurrent_pushes_with_checkpoints() {
    let p = pair();
    let mut all = vec![p.base.clone()];
    let mut ckpts: Vec<(String, u64, Vec<u8>)> = vec![];
    for i in 0..20u64 {
        for (tag, r, seed) in [("a", &p.a, 1000 + i), ("b", &p.b, 2000 + i)] {
            // Mix Tier-1 objects (< 1 MB) and chunked files (> 1 MB).
            let len = if i % 2 == 0 { 64 * 1024 } else { MB + MB / 4 };
            let data = prng_bytes(len, seed);
            let src = r.aux.join(format!("{tag}{i}.pt"));
            std::fs::write(&src, &data).unwrap();
            r.write("config.yaml", format!("lr: {tag}{i}\n"));
            let step = 10 + i;
            let id = r.run(
                &["-m", &format!("{tag}{i}")],
                &format!(
                    "emit {step} loss=1; cp {} \"$POLLARD_CKPT_DIR/step{step}.pt\"",
                    src.display()
                ),
            );
            all.push(id.clone());
            ckpts.push((id, step, data));
        }
        let (oa, ob) = push_together(&p.a, &p.b);
        assert!(oa.ok(), "round {i}: A push failed\n{oa}");
        assert!(ob.ok(), "round {i}: B push failed\n{ob}");
    }
    let c = assert_all_three_agree(&p, &all);
    for k in [0, 7, 18, 29, 39] {
        let (id, step, data) = &ckpts[k];
        let s = step.to_string();
        c.ok(&["fork", id, "--step", &s, "--no-sync"]);
        let got = std::fs::read(c.root.join(format!("ckpt/step{step}.pt")))
            .unwrap_or_else(|e| panic!("{id}: ckpt/step{step}.pt not restored: {e}"));
        assert!(got == *data, "{id}: restored checkpoint differs");
    }
}

/// F10 pins, D19-dependent (see D19_LOSSLESS_PINS). A pins x as `ax` and unpins `old-a`;
/// B pins y as `by` and unpins `old-b`; simultaneous push; then A, B, C pull.
#[test]
fn f10_concurrent_pin_edits() {
    let p = pair();
    p.a.ok(&["pin", &p.base, "old-a"]);
    p.a.ok(&["push"]);
    p.b.ok(&["pull"]);
    p.b.ok(&["pin", &p.base, "old-b"]);
    p.b.ok(&["push"]);
    p.a.ok(&["pull"]);
    for r in [&p.a, &p.b] {
        for pin in ["old-a", "old-b"] {
            assert_eq!(
                first_id(&r.ok(&["show", pin]).stdout),
                Some(p.base.clone()),
                "setup: {pin}"
            );
        }
    }
    p.a.write("config.yaml", "lr: x\n");
    let x = p.a.run(&["-m", "x"], "true");
    p.a.ok(&["pin", &x, "ax"]);
    p.a.ok(&["unpin", "old-a"]);
    p.b.write("config.yaml", "lr: y\n");
    let y = p.b.run(&["-m", "y"], "true");
    p.b.ok(&["pin", &y, "by"]);
    p.b.ok(&["unpin", "old-b"]);
    let a_map = pin_map(&p.a);
    let b_map = pin_map(&p.b);
    let (oa, ob) = push_together(&p.a, &p.b);
    assert!(oa.ok() && ob.ok(), "{oa}\n{ob}");
    let c = assert_all_three_agree(&p, &[p.base.clone(), x.clone(), y.clone()]);
    for (name, r) in [("A", &p.a), ("B", &p.b), ("C", &c)] {
        if D19_LOSSLESS_PINS {
            assert_eq!(
                first_id(&r.ok(&["show", "ax"]).stdout),
                Some(x.clone()),
                "{name}: ax"
            );
            assert_eq!(
                first_id(&r.ok(&["show", "by"]).stdout),
                Some(y.clone()),
                "{name}: by"
            );
            r.fail(&["show", "old-a"]);
            r.fail(&["show", "old-b"]);
        } else {
            let m = pin_map(r);
            assert!(
                m == a_map || m == b_map,
                "{name}: pin map is a mix of A's and B's:\n{m}"
            );
        }
    }
}

/// F10 pins (D19 default): two changes to the *same* pin are last-writer-wins by segment
/// order; all clones agree, no error.
#[test]
fn f10_same_pin_name_last_writer_wins() {
    let p = pair();
    p.a.write("config.yaml", "lr: x\n");
    let x = p.a.run(&["-m", "x"], "true");
    p.b.write("config.yaml", "lr: y\n");
    let y = p.b.run(&["-m", "y"], "true");
    p.a.ok(&["pin", &x, "best"]);
    p.b.ok(&["pin", &y, "best"]);
    let (oa, ob) = push_together(&p.a, &p.b);
    assert!(oa.ok() && ob.ok(), "{oa}\n{ob}");
    let c = assert_all_three_agree(&p, &[p.base.clone(), x.clone(), y.clone()]);
    let got: Vec<Option<String>> = [&p.a, &p.b, &c]
        .iter()
        .map(|r| first_id(&r.ok(&["show", "best"]).stdout))
        .collect();
    assert!(
        got[0] == got[1] && got[1] == got[2],
        "clones disagree on `best`: {got:?}"
    );
    assert!(got[0] == Some(x) || got[0] == Some(y), "{got:?}");
}
