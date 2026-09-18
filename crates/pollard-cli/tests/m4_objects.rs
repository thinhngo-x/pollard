//! M4: CDC chunking, weights manifests, `ckpt`, `artifact`, `fork --step`, `prune`, `gc`.
//! Uses a fake 50 MB checkpoint generated at test time (§8).
mod common;
use common::*;
use std::path::PathBuf;

/// Find a file by name anywhere under the working copy (excluding .pollard/).
fn find(r: &Repo, name: &str) -> Option<PathBuf> {
    fn go(d: &std::path::Path, name: &str) -> Option<PathBuf> {
        for e in std::fs::read_dir(d).ok()? {
            let p = e.ok()?.path();
            if p.file_name().is_some_and(|f| f == ".pollard") {
                continue;
            }
            if p.is_dir() {
                if let Some(x) = go(&p, name) {
                    return Some(x);
                }
            } else if p.file_name().is_some_and(|f| f == name) {
                return Some(p);
            }
        }
        None
    }
    go(&r.root, name)
}

/// Run a node that copies `src` into $POLLARD_CKPT_DIR/<name> and logs `step`.
fn run_ckpt(r: &Repo, note: &str, src: &std::path::Path, name: &str, step: u32) -> String {
    r.run(
        &["-m", note],
        &format!(
            "emit {step} loss=1; cp {} \"$POLLARD_CKPT_DIR/{name}\"",
            src.display()
        ),
    )
}

fn ckpt_fixture(r: &Repo, name: &str, data: &[u8]) -> PathBuf {
    let p = r.aux.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn dir_bytes(d: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(d) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_bytes(&p)
            } else {
                p.metadata().unwrap().len()
            }
        })
        .sum()
}

/// §9 M4 (v3): "Two checkpoints differing in one contiguous region of 1 % of bytes (in place, same
/// total size) share ≥ 95 % of chunks, by count and by bytes".
#[test]
fn checkpoints_differing_by_1pct_share_95pct_of_chunks() {
    let r = Repo::init();
    let a = fake_ckpt(1);
    let b = perturb(&a, 0.01, 1, 2);
    assert_eq!(a.len(), b.len());
    let diff = a.iter().zip(&b).filter(|(x, y)| x != y).count() as f64 / a.len() as f64;
    assert!((0.009..=0.0101).contains(&diff), "fixture sanity: {diff}");
    let pa = ckpt_fixture(&r, "a.pt", &a);
    let pb = ckpt_fixture(&r, "b.pt", &b);
    run_ckpt(&r, "A", &pa, "step100.pt", 100);
    let chunks = r.root.join(".pollard/chunks");
    let (n_a, bytes_a) = (r.chunk_count(), dir_bytes(&chunks));
    // 50 MB at 64 KB target ≈ 800 chunks; anything wildly off means files are stored whole.
    assert!(
        n_a >= 300,
        "only {n_a} chunk files for a 50 MB checkpoint; expected ~800 (target 64 KB, max 128 KB)"
    );
    r.write("config.yaml", "lr: 1\n");
    run_ckpt(&r, "B", &pb, "step100.pt", 100);
    let new_chunks = r.chunk_count() - n_a;
    let new_bytes = dir_bytes(&chunks) - bytes_a;
    let shared = 1.0 - new_chunks as f64 / n_a as f64;
    let shared_b = 1.0 - new_bytes as f64 / bytes_a as f64;
    assert!(
        shared >= 0.95,
        "B added {new_chunks} new chunks on top of A's {n_a}: shared by count {:.1}% < 95%",
        shared * 100.0
    );
    assert!(
        shared_b >= 0.95,
        "B added {new_bytes} chunk bytes on top of A's {bytes_a}: shared by bytes {:.1}% < 95%",
        shared_b * 100.0
    );
}

/// §9 M4: "`fork --step` restores a byte-identical file".
#[test]
fn fork_step_restores_byte_identical_checkpoint() {
    let r = Repo::init();
    let data = fake_ckpt(3);
    let src = ckpt_fixture(&r, "src.pt", &data);
    let a = run_ckpt(&r, "A", &src, "step100.pt", 100);
    // Remove any copy left in the working copy so the restore must come from the chunk store.
    while let Some(p) = find(&r, "step100.pt") {
        std::fs::remove_file(p).unwrap();
    }
    let o = r.ok(&["fork", &a, "--step", "100", "--no-sync"]);
    assert_eq!(o.last_line(), a, "{o}");
    let p = find(&r, "step100.pt")
        .unwrap_or_else(|| panic!("step100.pt not restored anywhere in the working copy:\n{o}"));
    assert!(
        std::fs::read(&p).unwrap() == data,
        "restored {} is not byte-identical",
        p.display()
    );
}

#[test]
fn fork_step_env_points_resumed_run_at_restored_checkpoint() {
    let r = Repo::init();
    let src = ckpt_fixture(&r, "s.pt", &prng_bytes(3 * MB, 9));
    let a = run_ckpt(&r, "A", &src, "step100.pt", 100);
    r.ok(&["fork", &a, "--step", "100", "--no-sync"]);
    r.write("config.yaml", "lr: 1\n");
    let res = r.aux.join("seen.txt");
    r.run(
        &["-m", "B"],
        &format!(
            "cmp \"$POLLARD_CKPT_DIR/step100.pt\" {} && echo SAME > {}",
            src.display(),
            res.display()
        ),
    );
    assert_eq!(
        std::fs::read_to_string(res).unwrap_or_default().trim(),
        "SAME",
        "resumed run does not find the restored checkpoint in $POLLARD_CKPT_DIR"
    );
}

#[test]
fn ckpt_command_registers_immediately_from_inside_script() {
    let r = Repo::init();
    let src = ckpt_fixture(&r, "c.pt", &prng_bytes(2 * MB, 4));
    let res = r.aux.join("show.txt");
    let id = r.run(&["-m", "A"], &format!(
        "cp {} \"$POLLARD_CKPT_DIR/step5.pt\"; \"$POLLARD_BIN\" ckpt \"$POLLARD_CKPT_DIR/step5.pt\"; \"$POLLARD_BIN\" show \"$POLLARD_NODE_ID\" > {}",
        src.display(), res.display()
    ));
    let during = std::fs::read_to_string(&res).unwrap();
    assert!(
        during.contains("step5"),
        "checkpoint not visible in `show` right after `pollard ckpt` (during run):\n{during}"
    );
    assert!(r.ok(&["show", &id]).stdout.contains("step5"));
}

#[test]
fn artifact_command_stores_output_under_weights_manifest() {
    let r = Repo::init();
    let id = r.run(&["-m", "A"], "mkdir -p outputs; head -c 3000000 /dev/urandom > outputs/preds.bin; \"$POLLARD_BIN\" artifact outputs/preds.bin");
    let s = r.ok(&["show", &id]).stdout;
    assert!(s.contains("preds.bin"), "artifact not listed in show:\n{s}");
    assert!(
        r.chunk_count() > 0,
        "a 3 MB artifact (>1 MB) must be chunked"
    );
}

#[test]
fn large_file_never_stored_whole() {
    let r = Repo::init();
    let src = ckpt_fixture(&r, "w.pt", &prng_bytes(5 * MB, 5));
    run_ckpt(&r, "A", &src, "w.pt", 1);
    let mut biggest = 0u64;
    fn go(d: &std::path::Path, m: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    go(&p, m)
                } else {
                    *m = (*m).max(p.metadata().unwrap().len())
                }
            }
        }
    }
    go(&r.root.join(".pollard/chunks"), &mut biggest);
    go(&r.root.join(".pollard/objects"), &mut biggest);
    assert!(
        biggest <= 1024 * 1024,
        "a {biggest}-byte blob in the store: files > 1 MB must be chunked (max chunk 128 KB)"
    );
}

/// §9 M4: "`prune` then `gc` frees only the pruned checkpoint's unique chunks".
#[test]
fn prune_then_gc_frees_only_pruned_unique_chunks() {
    let r = Repo::init();
    let a_bytes = fake_ckpt(11);
    let b_bytes = perturb(&a_bytes, 0.01, 1, 12);
    let pa = ckpt_fixture(&r, "a.pt", &a_bytes);
    let pb = ckpt_fixture(&r, "b.pt", &b_bytes);
    let a = run_ckpt(&r, "A", &pa, "step100.pt", 100);
    r.ok(&["gc"]);
    let n_a = r.chunk_count();
    r.write("config.yaml", "lr: 1\n");
    let b = run_ckpt(&r, "B", &pb, "step100.pt", 100);
    let n_ab = r.chunk_count();
    assert!(n_ab > n_a);
    let o = r.ok(&["prune", &b]);
    assert_eq!(o.last_line(), b, "prune must print the node id last:\n{o}");
    assert_eq!(
        r.chunk_count(),
        n_ab,
        "prune alone must not delete chunks (gc does)"
    );
    r.ok(&["gc"]);
    assert_eq!(
        r.chunk_count(),
        n_a,
        "after prune+gc chunk count should return to A's {n_a}"
    );
    // A's checkpoint is intact; B's recipe and metrics stay.
    while let Some(p) = find(&r, "step100.pt") {
        std::fs::remove_file(p).unwrap();
    }
    r.ok(&["fork", &a, "--step", "100", "--no-sync"]);
    assert!(
        std::fs::read(find(&r, "step100.pt").unwrap()).unwrap() == a_bytes,
        "gc damaged A's checkpoint"
    );
    let sb = r.ok(&["show", &b]).stdout;
    assert!(sb.contains("pruned"), "{sb}");
    assert!(
        r.ok(&["log", &b, "--key", "loss"]).stdout.contains("100"),
        "pruned node's metrics must remain queryable"
    );
}

#[test]
fn prune_keep_weights_keeps_chunks() {
    let r = Repo::init();
    let p = ckpt_fixture(&r, "a.pt", &prng_bytes(4 * MB, 21));
    let a = run_ckpt(&r, "A", &p, "a.pt", 1);
    let n = r.chunk_count();
    r.ok(&["prune", &a, "--keep-weights"]);
    r.ok(&["gc"]);
    assert_eq!(r.chunk_count(), n, "--keep-weights chunks were collected");
}

#[test]
fn prune_marks_whole_subtree_and_undo_restores() {
    let r = Repo::init();
    let a = r.run(&["-m", "A"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "B"], "true");
    r.write("config.yaml", "lr: 2\n");
    let c = r.run(&["-m", "C"], "true");
    r.ok(&["prune", &b]);
    assert!(
        r.ok(&["show", &c]).stdout.contains("pruned"),
        "descendant C not pruned with B"
    );
    assert!(!r.ok(&["show", &a]).stdout.contains("pruned"));
    let t = r.ok(&["tree"]).stdout;
    assert!(
        !t.contains(&c),
        "pruned subtree not collapsed in default tree:\n{t}"
    );
    let ta = r.ok(&["tree", "--all"]).stdout;
    assert!(
        ta.contains(&b) && ta.contains(&c),
        "tree --all hides pruned nodes:\n{ta}"
    );
    r.ok(&["undo"]);
    assert!(
        !r.ok(&["show", &b]).stdout.contains("pruned"),
        "undo did not un-prune B"
    );
    assert!(
        !r.ok(&["show", &c]).stdout.contains("pruned"),
        "undo did not un-prune C"
    );
}

#[test]
fn pruned_recipe_can_be_rerun_without_force() {
    // §4: duplicate check is against *non-pruned* nodes.
    let r = Repo::init();
    let a = r.run(&["-m", "A"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "B"], "true");
    r.ok(&["prune", &b]);
    r.ok(&["fork", &a, "--no-sync"]);
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "B again"], "true");
}

#[test]
fn siblings_exclude_pruned_unless_all() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    r.write("config.yaml", "lr: 1\n");
    let c1 = r.run(&["-m", "c1", "--parent", &p], "true");
    r.write("config.yaml", "lr: 2\n");
    let c2 = r.run(&["-m", "c2", "--parent", &p], "true");
    r.ok(&["prune", &c1]);
    let s = r.ok(&["siblings", &p]).stdout;
    assert!(
        !s.contains(&c1) && s.contains(&c2),
        "pruned child shown without --all:\n{s}"
    );
    let sa = r.ok(&["siblings", &p, "--all"]).stdout;
    assert!(sa.contains(&c1), "pruned child missing with --all:\n{sa}");
}

#[test]
fn gc_is_safe_on_fresh_repo_and_auto_flag() {
    let r = Repo::init();
    r.ok(&["gc"]);
    r.ok(&["gc", "--auto"]);
}

/// Review item: the CDC insertion-stability property test must exist in pollard-objects (§8).
#[test]
fn cdc_insertion_stability_property_test_exists() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pollard-objects");
    let mut src = String::new();
    fn go(d: &std::path::Path, s: &mut String) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    go(&p, s)
                } else if p.extension().is_some_and(|x| x == "rs") {
                    s.push_str(&std::fs::read_to_string(p).unwrap())
                }
            }
        }
    }
    go(&dir.join("src"), &mut src);
    go(&dir.join("tests"), &mut src);
    assert!(
        src.contains("proptest!"),
        "no proptest! block in pollard-objects"
    );
    assert!(
        src.to_lowercase().contains("insert"),
        "no insertion-stability property in pollard-objects"
    );
}

/// v3 §4: no checkpoint exactly at N → the largest step ≤ N is used, fork_step set to it, notice.
#[test]
fn fork_step_uses_largest_checkpoint_at_or_below_n_with_notice() {
    let r = Repo::init();
    let a = r.run(&["-m", "A"], "for s in 10 20 30; do emit $s loss=$s; echo \"w$s\" > \"$POLLARD_CKPT_DIR/step$s.pt\"; done");
    std::fs::remove_dir_all(r.root.join("ckpt")).ok();
    let o = r.ok(&["fork", &a, "--step", "25", "--no-sync"]);
    assert!(
        o.all().contains("20"),
        "no notice that step 20 was used for --step 25:\n{o}"
    );
    assert_eq!(
        std::fs::read_to_string(r.root.join("ckpt/step20.pt")).unwrap_or_default(),
        "w20\n",
        "step20.pt not restored:\n{o}"
    );
    r.write("config.yaml", "lr: 1\n");
    let res = r.aux.join("fs.txt");
    r.run(
        &["-m", "B"],
        &format!("echo $POLLARD_FORK_STEP > {}", res.display()),
    );
    assert_eq!(
        std::fs::read_to_string(res).unwrap().trim(),
        "20",
        "fork_step should be 20, not 25"
    );
}

/// v3 §4: checkpoint lookup walks ancestors by the metric inheritance rule.
#[test]
fn fork_step_finds_checkpoint_in_ancestor() {
    let r = Repo::init();
    let a = r.run(
        &["-m", "A"],
        "emit 10 loss=1; echo w10 > \"$POLLARD_CKPT_DIR/step10.pt\"; emit 20 loss=1",
    );
    r.ok(&["fork", &a, "--step", "10", "--no-sync"]);
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "B"], "emit 30 loss=1");
    std::fs::remove_dir_all(r.root.join("ckpt")).ok();
    let o = r.ok(&["fork", &b, "--step", "10", "--no-sync"]);
    assert_eq!(
        std::fs::read_to_string(r.root.join("ckpt/step10.pt")).unwrap_or_default(),
        "w10\n",
        "ancestor A's step10.pt not found via B:\n{o}"
    );
}

/// v3 §3: `undo` of `prune` after `gc` restores the nodes but warns that the weights are gone.
#[test]
fn undo_prune_after_gc_warns_weights_gone() {
    let r = Repo::init();
    std::fs::write(r.aux.join("w.pt"), prng_bytes(3 * MB, 8)).unwrap();
    let a = run_ckpt(&r, "A", &r.aux.join("w.pt"), "w.pt", 1);
    r.ok(&["prune", &a]);
    r.ok(&["gc"]);
    let o = r.ok(&["undo"]);
    assert!(
        !r.ok(&["show", &a]).stdout.contains("pruned"),
        "undo did not restore the node"
    );
    assert!(
        o.stderr.to_lowercase().contains("weight") || o.stderr.to_lowercase().contains("gc"),
        "no warning that weights are gone:\n{o}"
    );
}
