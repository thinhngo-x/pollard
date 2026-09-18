//! M7: local-path remote, `push`, `pull`, id salting. SPEC §5 "Remote".
//! S3 is not exercised (no S3 endpoint in CI); see TEST_REPORT.
mod common;
use common::*;
use std::path::Path;

/// (path, size) of every file under `dir`: a remote-side transfer fingerprint.
fn listing(dir: &Path) -> Vec<(String, u64)> {
    fn go(base: &Path, d: &Path, v: &mut Vec<(String, u64)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() { go(base, &p, v) } else { v.push((p.strip_prefix(base).unwrap().display().to_string(), p.metadata().unwrap().len())) }
        }
    }
    let mut v = vec![];
    go(dir, dir, &mut v);
    v.sort();
    v
}

fn clone_with_remote(remote: &Path) -> Repo {
    let r = Repo::init();
    r.set_config("remote", &format!("{:?}", remote.display().to_string()));
    r
}

/// §9 M7: "Two clones push children of the same parent to a local-path remote; both `pull` and
/// see all nodes with no collisions; a second `push` transfers zero bytes".
#[test]
fn two_clones_push_children_of_same_parent_no_collisions() {
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    let r1 = clone_with_remote(&remote);
    let p = r1.run(&["-m", "parent"], "emit 1 loss=1");
    r1.ok(&["push"]);
    for f in ["nodes.jsonl", "objects"] {
        assert!(remote.join(f).exists(), "remote lacks {f} after push (§5 layout)");
    }

    let r2 = clone_with_remote(&remote);
    r2.ok(&["pull"]);
    assert!(r2.ok(&["tree"]).stdout.contains(&p), "clone 2 does not see {p} after pull");

    // Both clones make a child of the same parent with the same next counter position.
    r1.ok(&["fork", &p, "--no-sync"]);
    r1.write("config.yaml", "lr: 1\n");
    let c1 = r1.run(&["-m", "from clone 1"], "true");
    r2.ok(&["fork", &p, "--no-sync"]);
    r2.write("config.yaml", "lr: 2\n");
    let c2 = r2.run(&["-m", "from clone 2"], "true");
    assert_ne!(c1, c2, "id collision between clones (ids must be salted per clone)");
    r1.ok(&["push"]);
    r2.ok(&["push"]);
    r1.ok(&["pull"]);
    r2.ok(&["pull"]);
    for (name, r) in [("clone1", &r1), ("clone2", &r2)] {
        let t = r.ok(&["tree"]).stdout;
        for id in [&p, &c1, &c2] {
            assert!(t.contains(id.as_str()), "{name} missing {id} after pull:\n{t}");
        }
        assert!(r.ok(&["show", &c1]).stdout.contains(&p) && r.ok(&["show", &c2]).stdout.contains(&p), "{name}: parent links lost in sync");
    }

    // Second push from each: zero bytes transferred → the remote is byte-for-byte unchanged.
    let before = listing(&remote);
    let o1 = r1.ok(&["push"]);
    let o2 = r2.ok(&["push"]);
    assert_eq!(listing(&remote), before, "second push changed the remote:\n{o1}\n{o2}");
}

#[test]
fn push_pull_transfers_checkpoints_and_second_push_is_noop() {
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().to_path_buf();
    let r1 = clone_with_remote(&remote);
    let data = prng_bytes(5 * MB, 77);
    std::fs::write(r1.aux.join("c.pt"), &data).unwrap();
    let a = r1.run(&["-m", "A"], &format!("emit 10 loss=1; cp {} \"$POLLARD_CKPT_DIR/step10.pt\"", r1.aux.join("c.pt").display()));
    r1.ok(&["push"]);
    let after_first = listing(&remote);
    let bytes: u64 = after_first.iter().map(|x| x.1).sum();
    assert!(bytes >= 4 * MB as u64, "push uploaded only {bytes} bytes; the 5 MB checkpoint's chunks are missing");
    r1.ok(&["push"]);
    assert_eq!(listing(&remote), after_first, "second push transferred data");

    let r2 = clone_with_remote(&remote);
    r2.ok(&["pull"]);
    r2.ok(&["fork", &a, "--step", "10", "--no-sync"]);
    let restored = std::fs::read(r2.root.join("ckpt/step10.pt")).expect("ckpt/step10.pt not restored in clone 2");
    assert!(restored == data, "checkpoint pulled from remote is not byte-identical");
}

#[test]
fn push_uploads_only_missing_chunks() {
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().to_path_buf();
    let r = clone_with_remote(&remote);
    let a = prng_bytes(8 * MB, 1);
    let b = perturb(&a, 0.01, 2, 2);
    std::fs::write(r.aux.join("a.pt"), &a).unwrap();
    std::fs::write(r.aux.join("b.pt"), &b).unwrap();
    r.run(&["-m", "A"], &format!("cp {} \"$POLLARD_CKPT_DIR/w.pt\"", r.aux.join("a.pt").display()));
    r.ok(&["push"]);
    let s1: u64 = listing(&remote).iter().map(|x| x.1).sum();
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "B"], &format!("cp {} \"$POLLARD_CKPT_DIR/w.pt\"", r.aux.join("b.pt").display()));
    r.ok(&["push"]);
    let s2: u64 = listing(&remote).iter().map(|x| x.1).sum();
    assert!(s2 - s1 < (a.len() / 5) as u64, "second push uploaded {} bytes for a 1%-different 8 MB checkpoint", s2 - s1);
}

#[test]
fn notes_and_pins_sync_last_writer_wins() {
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().to_path_buf();
    let r1 = clone_with_remote(&remote);
    let a = r1.run(&["-m", "orig"], "true");
    r1.ok(&["pin", &a, "best"]);
    r1.ok(&["push"]);
    let r2 = clone_with_remote(&remote);
    r2.ok(&["pull"]);
    assert_eq!(first_id(&r2.ok(&["show", "best"]).stdout).as_deref(), Some(a.as_str()), "pin not pulled");
    r2.ok(&["note", &a, "renamed in clone 2"]);
    r2.ok(&["push"]);
    r1.ok(&["pull"]);
    assert!(r1.ok(&["show", &a]).stdout.contains("renamed in clone 2"), "note update did not sync (last writer wins)");
}

#[test]
fn push_without_remote_fails_clearly() {
    let r = Repo::init();
    let o = r.fail(&["push"]);
    assert!(o.stderr.to_lowercase().contains("remote"), "{o}");
}
