//! M3: run protocol (env vars, JSONL tailing, POLLARD_CONFIG), metrics table, inheritance,
//! fork-step rule, `log`, metric rows in `siblings`. SPEC §4 run protocol, §5 tier 3, §6 step 3.
mod common;
use common::*;

/// Parse `pollard log` output into (step, value) points: every line whose first numeric token
/// is an integer step followed by a value.
fn points(text: &str) -> Vec<(i64, f64)> {
    text.lines()
        .filter_map(|l| {
            let n = numbers(l);
            (n.len() >= 2 && n[0].fract() == 0.0 && n[0] >= 0.0).then(|| (n[0] as i64, n[1]))
        })
        .collect()
}

#[test]
fn run_sets_protocol_env_vars() {
    let r = Repo::init();
    let envf = r.aux.join("env.txt");
    let id = r.run(&["-m", "a"], &format!(
        "echo \"id=$POLLARD_NODE_ID\" > {e}; echo \"metrics=$POLLARD_METRICS\" >> {e}; echo \"ckpt=$POLLARD_CKPT_DIR\" >> {e}; echo \"fork=${{POLLARD_FORK_STEP-UNSET}}\" >> {e}; test -d \"$POLLARD_CKPT_DIR\"",
        e = envf.display()
    ));
    let env = std::fs::read_to_string(&envf).unwrap();
    assert!(env.contains(&format!("id={id}\n")), "POLLARD_NODE_ID != printed id {id}:\n{env}");
    assert!(!env.contains("metrics=\n"), "POLLARD_METRICS unset:\n{env}");
    assert!(!env.contains("ckpt=\n"), "POLLARD_CKPT_DIR unset:\n{env}");
    assert!(env.contains("fork=UNSET"), "POLLARD_FORK_STEP must be unset on a fresh run:\n{env}");
}

/// Journey A: "`run` prints the new node id before the script starts".
#[test]
fn run_prints_node_id_before_script_starts() {
    let r = Repo::init();
    let o = r.run_out(&["-m", "a"], "echo SCRIPT-STARTED");
    assert!(o.ok(), "{o}");
    let id = o.last_line();
    let first_id_pos = o.stdout.find(&id);
    let started = o.stdout.find("SCRIPT-STARTED").expect("script stdout not passed through");
    assert!(first_id_pos.is_some_and(|p| p < started), "node id not printed before the script's output:\n{o}");
}

/// §9 M3: "A shell script that echoes JSONL lines gets its metrics ingested live".
#[test]
fn jsonl_metrics_ingested_live_while_running() {
    let r = Repo::init();
    let res = r.aux.join("live.txt");
    let id = r.run(&["-m", "live"], &format!(
        r#"emit 1 loss=0.5
emit 2 loss=0.25
i=0; out=NOT-LIVE
while [ $i -lt 100 ]; do
  if "$POLLARD_BIN" log "$POLLARD_NODE_ID" --key loss 2>/dev/null | grep -q '0.25'; then out=LIVE; break; fi
  i=$((i+1)); sleep 0.1
done
echo $out > {res}
emit 3 loss=0.125"#,
        res = res.display()
    ));
    assert_eq!(std::fs::read_to_string(&res).unwrap().trim(), "LIVE", "metrics were not visible via `log` during the run (10 s budget)");
    let pts = points(&r.ok(&["log", &id, "--key", "loss"]).stdout);
    assert_eq!(pts, vec![(1, 0.5), (2, 0.25), (3, 0.125)], "remainder not ingested on exit");
}

#[test]
fn metrics_ingested_on_exit_multiple_keys_per_line() {
    let r = Repo::init();
    let id = r.run(&["-m", "a"], "emit 10 loss=2.5 acc=0.1; emit 20 loss=2.0 acc=0.2");
    let loss = points(&r.ok(&["log", &id, "--key", "loss"]).stdout);
    let acc = points(&r.ok(&["log", &id, "--key", "acc"]).stdout);
    assert_eq!(loss, vec![(10, 2.5), (20, 2.0)]);
    assert_eq!(acc, vec![(10, 0.1), (20, 0.2)]);
}

/// §9 M3: "fork A→B at step 100, B logs 101–200, `log B` returns 200 points";
/// "forking B at step 50 re-parents to A with a notice".
#[test]
fn fork_step_inheritance_and_monotonicity() {
    let r = Repo::init();
    // A logs 1..150; value = step. Points 101..150 of A must NOT leak into B.
    let a = r.run(&["-m", "A"], "s=1; while [ $s -le 150 ]; do emit $s loss=$s; s=$((s+1)); done");
    let o = r.ok(&["fork", &a, "--step", "100", "--no-sync"]);
    assert_eq!(o.last_line(), a, "fork must print the node id last:\n{o}");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    let envf = r.aux.join("fs.txt");
    let b = r.run(&["-m", "B"], &format!(
        "echo \"$POLLARD_FORK_STEP\" > {}; s=101; while [ $s -le 200 ]; do emit $s loss=$((s+1000)); s=$((s+1)); done",
        envf.display()
    ));
    assert_eq!(std::fs::read_to_string(&envf).unwrap().trim(), "100", "POLLARD_FORK_STEP not set to 100 in B");
    let sb = r.ok(&["show", &b]).stdout;
    assert!(sb.contains(&a), "B's parent is not A:\n{sb}");

    let pts = points(&r.ok(&["log", &b, "--key", "loss"]).stdout);
    assert_eq!(pts.len(), 200, "log B should return 200 points, got {}", pts.len());
    let steps: Vec<i64> = pts.iter().map(|p| p.0).collect();
    assert_eq!(steps, (1..=200).collect::<Vec<_>>(), "steps not the continuous 1..=200");
    assert!(pts.contains(&(100, 100.0)), "step 100 should be A's value");
    assert!(pts.contains(&(150, 1150.0)), "step 150 should be B's own value, not A's");

    // Fork B at step 50 < B.fork_step(100): re-parent to A (who actually logged step 50), with notice.
    r.write("config.yaml", "lr: 99\n"); // uncommitted edit; fork must replace it with A's recipe
    let o = r.ok(&["fork", &b, "--step", "50", "--no-sync"]);
    assert!(o.all().contains(&a), "no one-line notice naming the re-parent target {a}:\n{o}");
    // v3 §5 / D-21: behaves exactly like `fork A --step 50` → working copy gets A's recipe.
    assert_eq!(r.read("config.yaml"), "lr: 3\ndepth: 12\nact: relu\n", "re-parented fork did not restore A's config");
    assert_eq!(r.current(), a, "re-parented fork should make A current");
    r.write("config.yaml", "lr: 2\ndepth: 12\nact: relu\n");
    let c = r.run(&["-m", "C"], "emit 51 loss=7");
    let sc = r.ok(&["show", &c]).stdout;
    assert!(sc.contains(&a) && !sc.contains(&b), "C should be re-parented to A, not B:\n{sc}");
    let pc = points(&r.ok(&["log", &c, "--key", "loss"]).stdout);
    assert_eq!(pc.len(), 51, "C: 50 inherited + 1 own");
    assert!(pc.contains(&(50, 50.0)));
}

#[test]
fn node_may_log_key_parent_never_logged() {
    let r = Repo::init();
    let a = r.run(&["-m", "A"], "emit 1 loss=1; emit 2 loss=2");
    r.ok(&["fork", &a, "--step", "2", "--no-sync"]);
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "B"], "emit 3 loss=3 acc=0.9");
    assert_eq!(points(&r.ok(&["log", &b, "--key", "acc"]).stdout), vec![(3, 0.9)]);
    assert_eq!(points(&r.ok(&["log", &b, "--key", "loss"]).stdout).len(), 3);
}

#[test]
fn plain_child_without_fork_step_inherits_nothing() {
    let r = Repo::init();
    r.run(&["-m", "A"], "emit 1 loss=1");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "B"], "emit 5 loss=5");
    assert_eq!(points(&r.ok(&["log", &b, "--key", "loss"]).stdout), vec![(5, 5.0)]);
}

/// §9 M3: "`last_common` and `last_own` cells are correct" (§6 step 3).
#[test]
fn siblings_last_common_and_last_own_cells() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "s=10; while [ $s -le 100 ]; do emit $s loss=5; s=$((s+10)); done");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    // C1 stops at 50 with loss 4 → last_common = 50 (C3 failed, excluded).
    let c1 = r.run(&["-m", "c1", "--parent", &p], "s=10; while [ $s -le 50 ]; do emit $s loss=4; s=$((s+10)); done");
    r.write("config.yaml", "lr: 2\ndepth: 12\nact: relu\n");
    // C2 runs to 80: loss 3 through step 50, 2.5 at its final step.
    let c2 = r.run(&["-m", "c2", "--parent", &p], "s=10; while [ $s -le 70 ]; do emit $s loss=3; s=$((s+10)); done; emit 80 loss=2.5");
    r.write("config.yaml", "lr: 4\ndepth: 12\nact: relu\n");
    let o3 = r.run_out(&["-m", "c3", "--parent", &p], "emit 10 loss=9; exit 1");
    let c3 = o3.last_line();
    assert!(is_node_id(&c3), "{o3}");

    let out = r.ok(&["siblings", &p, "--metric", "loss"]).stdout;
    let metric_lines: Vec<&str> = out.lines().filter(|l| l.contains("loss")).collect();
    assert!(metric_lines.len() >= 2, "expected a last_common row and a last_own row for `loss`:\n{out}");
    // last_common row: label mentions step 50; C1 = 4 (−1), C2 = 3 (−2).
    let common = metric_lines.iter().find(|l| has_num(l, 3.0) && has_num(l, -2.0) && has_num(l, 4.0) && has_num(l, -1.0))
        .unwrap_or_else(|| panic!("no last_common row with C1=4(−1) and C2=3(−2):\n{out}"));
    assert!(common.contains("50"), "last_common row should be labelled with step 50:\n{out}");
    // last_own row: C1 = 4 at step 50, C2 = 2.5 at step 80 (−2.5 vs parent's 5 at 80).
    assert!(metric_lines.iter().any(|l| has_num(l, 2.5) && has_num(l, -2.5) && has_num(l, 4.0)),
        "no last_own row with C1=4 and C2=2.5(−2.5):\n{out}");
    assert!(out.contains("failed"), "status row should show C3 failed:\n{out}");
    let _ = (c1, c2);
}

#[test]
fn metric_key_defaults_to_first_alphabetically() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 10 zeta=1 alpha=1");
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "c", "--parent", &p], "emit 10 zeta=2 alpha=7");
    let out = r.ok(&["siblings", &p]).stdout;
    assert!(out.contains("alpha") && !out.contains("zeta"), "default metric should be `alpha`:\n{out}");
}

#[test]
fn metric_key_from_primary_metric_config() {
    let r = Repo::init();
    r.set_config("primary_metric", "\"zeta\"");
    let p = r.run(&["-m", "P"], "emit 10 zeta=1 alpha=1");
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "c", "--parent", &p], "emit 10 zeta=2 alpha=7");
    let out = r.ok(&["siblings", &p]).stdout;
    assert!(out.contains("zeta"), "primary_metric = zeta ignored:\n{out}");
}

/// §4: config capture `sdk` mode via POLLARD_CONFIG (default when no config.yaml).
#[test]
fn pollard_config_sdk_capture() {
    let r = Repo::bare();
    r.write("train.sh", "echo train\n");
    r.ok(&["init"]);
    let a = r.run(&["-m", "a"], "echo '{\"lr\": 0.1, \"opt\": {\"name\": \"adam\"}}' > \"$POLLARD_CONFIG\"; emit 1 loss=1");
    let b = r.run(&["-m", "b", "--force"], "echo '{\"lr\": 0.2, \"opt\": {\"name\": \"adam\"}}' > \"$POLLARD_CONFIG\"; emit 1 loss=1");
    let d = r.ok(&["diff", &a, &b]).stdout;
    assert!(d.contains("lr") && d.contains("0.1") && d.contains("0.2"), "config written via POLLARD_CONFIG not captured:\n{d}");
    assert!(!d.contains("opt.name"), "unchanged key in diff:\n{d}");
}

#[test]
fn log_accepts_node_refs() {
    let r = Repo::init();
    r.run(&["-m", "a"], "emit 1 loss=1");
    assert_eq!(points(&r.ok(&["log", "@", "--key", "loss"]).stdout), vec![(1, 1.0)]);
}

/// v3 §4: POLLARD_CKPT_DIR = `checkpoint_dir` (default `ckpt/`), auto-ignored from the code manifest.
#[test]
fn ckpt_dir_defaults_to_ckpt_and_is_not_code() {
    let r = Repo::init();
    let res = r.aux.join("dir.txt");
    r.run(&["-m", "a"], &format!("echo \"$POLLARD_CKPT_DIR\" > {}; echo w > \"$POLLARD_CKPT_DIR/step1.pt\"", res.display()));
    let d = std::fs::read_to_string(&res).unwrap();
    let d = std::path::Path::new(d.trim());
    let d = if d.is_absolute() { d.to_path_buf() } else { r.root.join(d) };
    assert_eq!(d.canonicalize().ok(), r.root.join("ckpt").canonicalize().ok(), "POLLARD_CKPT_DIR is not <repo>/ckpt");
    r.write("ckpt/step2.pt", "more weights");
    let o = r.run_out(&["-m", "b"], "true");
    assert!(!o.ok(), "files in ckpt/ changed the recipe (must be auto-ignored):\n{o}");
}
