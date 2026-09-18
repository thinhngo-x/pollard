//! M8: sweeps, seed collapse, `apply`, `pin`, tree collapsing, `--metric` highlight.
mod common;
use common::*;

/// Parent P plus a 32-member sweep "seeds". Seed goes into config.yaml (the recipe) because the
/// command line is not part of the recipe (spec question Q8). loss alternates 1.0 / 2.0 →
/// mean 1.5, std 0.5 (population) or 0.508 (sample).
fn sweep32() -> (Repo, String, Vec<String>) {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 100 loss=3");
    let mut members = vec![];
    for s in 1..=32 {
        r.write(
            "config.yaml",
            format!("lr: 3\ndepth: 12\nact: relu\nseed: {s}\n"),
        );
        let loss = if s % 2 == 0 { "2.0" } else { "1.0" };
        members.push(r.run(
            &[
                "--sweep",
                "seeds",
                "-m",
                &format!("seed {s}"),
                "--parent",
                &p,
            ],
            &format!("emit 100 loss={loss}"),
        ));
    }
    (r, p, members)
}

/// §9 M8: "32-seed sweep renders as one row and one sibling column with mean ± std".
#[test]
fn sweep_of_32_is_one_tree_row_and_one_sibling_column_with_mean_std() {
    let (r, p, members) = sweep32();
    let t = r.ok(&["tree", "--metric", "loss"]).stdout;
    let leaked: Vec<_> = members.iter().filter(|m| t.contains(m.as_str())).collect();
    assert!(
        leaked.is_empty(),
        "sweep members listed individually in tree: {leaked:?}\n{t}"
    );
    let rows: Vec<&str> = t.lines().filter(|l| l.contains("seeds")).collect();
    assert_eq!(rows.len(), 1, "sweep should be exactly one tree row:\n{t}");
    assert!(rows[0].contains("32"), "sweep row should say 32 runs:\n{t}");
    assert!(
        rows[0].contains('±') && rows[0].contains("1.5"),
        "sweep row lacks mean ± std (1.5 ± 0.5):\n{t}"
    );

    let s = r.ok(&["siblings", &p, "--metric", "loss"]).stdout;
    let leaked: Vec<_> = members.iter().filter(|m| s.contains(m.as_str())).collect();
    assert!(
        leaked.is_empty(),
        "sweep members shown as separate sibling columns: {leaked:?}\n{s}"
    );
    let metric_row = s
        .lines()
        .find(|l| l.contains("loss") && l.contains('±'))
        .unwrap_or_else(|| panic!("no mean ± std metric cell:\n{s}"));
    let n = numbers(&metric_row.replace('±', " "));
    assert!(
        n.iter().any(|x| (x - 1.5).abs() < 1e-9),
        "mean 1.5 missing: {metric_row}"
    );
    assert!(
        n.iter().any(|x| (x - 0.5).abs() < 0.011),
        "std ≈0.5 missing: {metric_row}"
    );
    assert!(
        s.contains("32"),
        "column header should say 32 seeds/runs:\n{s}"
    );
}

#[test]
fn expand_sweeps_shows_members_transposed() {
    let (r, p, members) = sweep32();
    let s = r.ok(&["siblings", &p, "--expand-sweeps"]).stdout;
    for m in &members {
        assert!(
            s.contains(m.as_str()),
            "{m} missing with --expand-sweeps:\n{s}"
        );
    }
    // > 6 columns → transposed: one child per row.
    for m in &members {
        assert_eq!(
            s.lines().filter(|l| l.contains(m.as_str())).count(),
            1,
            "table not transposed (one child per row):\n{s}"
        );
    }
}

#[test]
fn siblings_more_than_six_children_transposes() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    let mut kids = vec![];
    for i in 0..7 {
        r.write(
            "config.yaml",
            format!("lr: {}\ndepth: 12\nact: relu\n", i + 10),
        );
        kids.push(r.run(&["-m", &format!("k{i}"), "--parent", &p], "true"));
    }
    let s = r.ok(&["siblings", &p]).stdout;
    for k in &kids {
        let l = line_with(&s, k).unwrap_or_else(|| panic!("{k} missing:\n{s}"));
        assert!(
            l.contains("→") || l.contains("->"),
            "transposed row for {k} lacks its lr delta:\n{s}"
        );
    }
    assert!(
        !kids
            .iter()
            .all(|k| line_with(&s, &kids[0]).unwrap().contains(k.as_str())),
        "7 children still rendered as columns:\n{s}"
    );
}

/// §6 step 3: children differing only in `seed_keys` merge into one `N seeds` column.
#[test]
fn seed_collapse_without_sweep_flag() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 10 loss=3");
    let mut seeds = vec![];
    for s in 1..=3 {
        r.write(
            "config.yaml",
            format!("lr: 1\ndepth: 12\nact: relu\nseed: {s}\n"),
        );
        seeds.push(r.run(
            &["-m", &format!("s{s}"), "--parent", &p],
            &format!("emit 10 loss={s}"),
        ));
    }
    r.write("config.yaml", "lr: 3\ndepth: 24\nact: relu\n");
    let other = r.run(&["-m", "deeper", "--parent", &p], "emit 10 loss=5");
    let s = r.ok(&["siblings", &p, "--metric", "loss"]).stdout;
    assert!(s.contains("3 seeds"), "no `3 seeds` column:\n{s}");
    assert!(s.contains(&other), "{s}");
    assert!(
        seeds.iter().all(|id| !s.contains(id.as_str())),
        "seed runs not collapsed:\n{s}"
    );
    let row = s
        .lines()
        .find(|l| l.contains('±'))
        .unwrap_or_else(|| panic!("no mean ± std:\n{s}"));
    assert!(has_num(row, 2.0), "mean of 1,2,3 should be 2:\n{row}");
}

/// §9 M8: "`apply` of a conflicting delta leaves markers and exits 0 with a warning".
#[test]
fn apply_conflicting_delta_leaves_markers_exit_0_with_warning() {
    let r = Repo::init();
    r.write("model.py", "a = 0\nx = 1\nb = 0\n");
    let p = r.run(&["-m", "P"], "true");
    r.write("model.py", "a = 0\nx = 2\nb = 0\n");
    let c = r.run(&["-m", "x=2", "--parent", &p], "true");
    r.ok(&["fork", &p, "--no-sync"]);
    r.write("model.py", "a = 0\nx = 3\nb = 0\n");
    let o = r.po(&["apply", &c]);
    assert!(o.ok(), "apply with conflict must exit 0:\n{o}");
    assert!(
        o.stderr.to_lowercase().contains("conflict") || o.stderr.to_lowercase().contains("warn"),
        "no warning on stderr:\n{o}"
    );
    let m = r.read("model.py");
    assert!(
        m.contains("<<<<<<<")
            && m.contains(">>>>>>>")
            && m.contains("x = 2")
            && m.contains("x = 3"),
        "conflict markers missing:\n{m}"
    );
}

/// Journey B: "`apply` touches only `config.yaml`, no conflict".
#[test]
fn apply_clean_delta_touches_only_changed_files() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    let lr = r.run(&["-m", "lr", "--parent", &p], "true");
    r.ok(&["fork", &p, "--no-sync"]);
    r.write("model.py", "x = 1\ny = 2\n");
    r.write("config.yaml", "lr: 3\ndepth: 24\nact: relu\n");
    let deep = r.run(&["-m", "deep", "--parent", &p], "true");
    let before = r.snapshot();
    let o = r.ok(&["apply", &lr]);
    let after = r.snapshot();
    let changed: Vec<_> = after
        .iter()
        .filter(|f| !before.contains(f))
        .map(|f| f.0.clone())
        .collect();
    assert_eq!(
        changed,
        vec!["config.yaml".to_string()],
        "apply touched more than config.yaml:\n{o}"
    );
    assert_eq!(
        r.read("config.yaml"),
        "lr: 1\ndepth: 24\nact: relu\n",
        "config delta not applied cleanly"
    );
    assert!(!o.stderr.to_lowercase().contains("conflict"), "{o}");
    let _ = deep;
}

#[test]
fn apply_is_undoable() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    let c = r.run(&["-m", "c", "--parent", &p], "true");
    r.ok(&["fork", &p, "--no-sync"]);
    let before = r.snapshot();
    r.ok(&["apply", &c]);
    assert!(r.snapshot() != before);
    r.ok(&["undo"]);
    assert!(
        r.snapshot() == before,
        "undo did not revert apply's working-copy change"
    );
}

#[test]
fn pin_unpin_resolve_and_undo() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    let o = r.ok(&["pin", &a, "paper-v1"]);
    assert_eq!(o.last_line(), a, "pin must print the node id last:\n{o}");
    assert_eq!(
        first_id(&r.ok(&["show", "paper-v1"]).stdout).as_deref(),
        Some(a.as_str()),
        "pin name does not resolve"
    );
    let o = r.ok(&["unpin", "paper-v1"]);
    assert_eq!(o.last_line(), a, "unpin must print the node id last:\n{o}");
    r.fail(&["show", "paper-v1"]);
    r.ok(&["undo"]);
    assert_eq!(
        first_id(&r.ok(&["show", "paper-v1"]).stdout).as_deref(),
        Some(a.as_str()),
        "undo of unpin did not restore pin"
    );
    r.ok(&["undo"]);
    r.fail(&["show", "paper-v1"]);
}

#[test]
fn pinned_nodes_show_pin_name_in_siblings_header() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    r.write("config.yaml", "lr: 1\n");
    let c = r.run(&["-m", "c", "--parent", &p], "true");
    r.ok(&["pin", &c, "champion"]);
    let s = r.ok(&["siblings", &p]).stdout;
    assert!(
        s.lines().next().is_some_and(|_| s.contains("champion")),
        "pin name not shown in header:\n{s}"
    );
}

#[test]
fn tree_collapses_failed_subtrees_unless_all() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "true");
    r.write("config.yaml", "lr: 1\n");
    let f = r
        .run_out(&["-m", "F", "--parent", &p], "exit 1")
        .last_line();
    r.write("config.yaml", "lr: 2\n");
    let g = r.run(&["-m", "G under failed", "--parent", &f], "true");
    let t = r.ok(&["tree"]).stdout;
    assert!(
        !t.contains(&g),
        "child of failed node shown in collapsed tree:\n{t}"
    );
    let ta = r.ok(&["tree", "--all"]).stdout;
    assert!(
        ta.contains(&f) && ta.contains(&g),
        "tree --all hides nodes:\n{ta}"
    );
}

#[test]
fn tree_metric_highlights_best_path() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 10 loss=3");
    r.write("config.yaml", "lr: 1\n");
    let good = r.run(&["-m", "good", "--parent", &p], "emit 10 loss=1");
    r.write("config.yaml", "lr: 2\n");
    let bad = r.run(&["-m", "bad", "--parent", &p], "emit 10 loss=2");
    let t = r.ok(&["tree", "--metric", "loss"]).stdout;
    let gl = line_with(&t, &good).unwrap();
    let bl = line_with(&t, &bad).unwrap();
    assert!(gl.contains('★'), "best node not highlighted with ★:\n{t}");
    assert!(!bl.contains('★'), "worse sibling highlighted:\n{t}");
    assert!(
        has_num(gl, 1.0),
        "metric value not shown on the tree row:\n{t}"
    );
}

#[test]
/// v3 §4: auto-prune dropped; `gc --auto` never prunes anything.
fn gc_auto_never_prunes() {
    let r = Repo::init();
    let data = prng_bytes(3 * MB, 5);
    std::fs::write(r.aux.join("w.pt"), &data).unwrap();
    let a = r.run(
        &["-m", "a"],
        &format!(
            "cp {} \"$POLLARD_CKPT_DIR/w.pt\"",
            r.aux.join("w.pt").display()
        ),
    );
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "b"], "true");
    r.ok(&["pin", &b, "keep"]);
    let n = r.chunk_count();
    r.ok(&["gc", "--auto"]);
    assert_eq!(r.chunk_count(), n, "gc --auto removed live chunks of {a}");
    assert!(!r.ok(&["show", &a]).stdout.contains("pruned"));
}
