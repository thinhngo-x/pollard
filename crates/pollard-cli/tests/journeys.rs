//! SPEC §10 user journeys A–D, end to end, scaled down: no GPU/PyTorch; `train.py` is plain
//! Python doing file I/O over the run protocol; the 10 GB dataset is a small `data/` dir.
//! Each journey replays the previous ones from scratch so failures stay attributable.
mod common;
use common::*;

const TRAIN_PY: &str = r#"
import os, sys, signal
cfg = {}
for line in open("config.yaml"):
    if ":" in line:
        k, v = line.split(":", 1)
        cfg[k.strip()] = v.strip()
lr = float(cfg.get("lr", "3e-4")); depth = int(cfg.get("depth", "12")); seed = int(cfg.get("seed", "0"))
for a in sys.argv[1:]:
    if a.startswith("seed="): seed = int(a[5:])
attn = "Attention" in open("model.py").read()
total = int(os.environ.get("TOTAL_STEPS", "50000")); kill_at = int(os.environ.get("KILL_AT", "0"))
start = int(os.environ.get("POLLARD_FORK_STEP") or 0)
ck = os.environ["POLLARD_CKPT_DIR"]
if start:
    assert os.path.exists(os.path.join(ck, "step%d.pt" % start)), "resume checkpoint missing"
base = 2.5 - (0.2 if depth > 12 else 0) - (0.1 if attn else 0) - (0.05 if lr < 3e-4 else 0) + 0.01 * (seed % 3)
m = open(os.environ["POLLARD_METRICS"], "a")
for step in range(start + 100, total + 1, 100):
    m.write('{"step": %d, "val_loss": %.6f}\n' % (step, base + 1.0 / (1 + step / 1000)))
    m.flush()
    if step % 10000 == 0:
        open(os.path.join(ck, "step%d.pt" % step), "wb").write(b"%d" % step * 20000)
    if kill_at and step >= kill_at:
        m.close()
        os.killpg(os.getpgrp(), signal.SIGINT)  # Maya hits Ctrl-C: SIGINT to the foreground group
        signal.pause()
"#;

const PYPROJECT: &str = "[project]\nname = \"maya\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = []\n\n[tool.uv]\npackage = false\n";

struct J {
    r: Repo,
    root: String,
    warm: String,
    cold: String,
    red: String,
    blue: String,
    gold: String,
}

fn po_env(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Out {
    let mut c = r.cmd_in(&r.root, &bin(), args);
    for (k, v) in env {
        c.env(k, v);
    }
    r.exec(c)
}

fn run_train(r: &Repo, pre: &[&str], env: &[(&str, &str)]) -> String {
    let mut args = vec!["run"];
    args.extend_from_slice(pre);
    args.push("train.py");
    let o = po_env(r, &args, env);
    assert!(o.ok(), "{o}");
    let id = o.last_line();
    assert!(is_node_id(&id), "{o}");
    id
}

/// Journey A: first day on an existing project.
fn journey_a() -> J {
    let r = Repo::bare();
    r.write("train.py", TRAIN_PY);
    r.write("config.yaml", "lr: 3e-4\ndepth: 12\n");
    r.write("model.py", "class Model:\n    depth = 12\n");
    r.write("pyproject.toml", PYPROJECT);
    r.write("REPORT.md", "# Report\n");
    r.write(".gitignore", ".venv/\n__pycache__/\n");
    write_200_files(&r);
    for i in 0..20 {
        r.write(&format!("data/shard{i}.bin"), prng_bytes(MB, i));
    }
    r.sh("uv lock --quiet");
    git_init_commit(&r);

    let o = r.ok(&["init", "--from-git"]);
    // Timing budget: spec < 5 s (release; see common::budget). Our data/ is 20 MB, not 10 GB (Q7).
    assert!(
        o.elapsed < budget(std::time::Duration::from_secs(5)),
        "init --from-git took {:?} (spec: < 5 s)",
        o.elapsed
    );
    let root = o.last_line();
    assert!(is_node_id(&root), "{o}");

    let o = po_env(&r, &["run", "-m", "baseline", "train.py"], &[]);
    assert!(o.ok(), "{o}");
    let warm = o.last_line();
    assert!(
        o.stdout.lines().next().is_some_and(|l| l.contains(&warm)),
        "run must print the node id first, before the script starts:\n{o}"
    );
    assert!(
        o.all().contains("uv run train.py"),
        "launch line should say `uv run train.py`:\n{o}"
    );

    let t = r.ok(&["tree"]).stdout;
    assert_eq!(ids_in(&t).len(), 2, "tree should show two nodes:\n{t}");
    let s = r.ok(&["show", &warm]).stdout;
    assert!(
        s.contains("done") && s.contains("val_loss"),
        "warm should be done with a final val_loss:\n{s}"
    );
    assert!(
        s.contains("step50000") || s.contains("50k") || s.contains("50000"),
        "warm's checkpoint missing from show:\n{s}"
    );
    J {
        r,
        root,
        warm,
        cold: String::new(),
        red: String::new(),
        blue: String::new(),
        gold: String::new(),
    }
}

/// Journey B: the exploration loop.
fn journey_b() -> J {
    let mut j = journey_a();
    let r = &j.r;
    r.ok(&["fork", &j.warm]);
    r.write(
        "config.yaml",
        r.read("config.yaml").replace("lr: 3e-4", "lr: 1e-4"),
    );
    j.cold = run_train(r, &[], &[]);
    let s = r.ok(&["show", &j.cold]).stdout;
    assert!(
        s.contains("auto") && s.contains("lr"),
        "cold should carry an auto note about lr:\n{s}"
    );
    assert!(s.contains(&j.warm), "cold's parent should be warm:\n{s}");

    r.ok(&["fork", &j.warm]);
    assert!(
        r.read("config.yaml").contains("lr: 3e-4"),
        "fork warm did not restore config"
    );
    r.write(
        "model.py",
        "class Attention: pass\nclass Model:\n    depth = 24\n",
    );
    r.write("config.yaml", "lr: 3e-4\ndepth: 24\n");
    r.write("REPORT.md", "# Report\nTried attention.\n");
    j.red = run_train(r, &["-m", "deeper + attn"], &[]);
    let sib = r.ok(&["siblings", &j.warm, "--metric", "val_loss"]).stdout;
    assert!(
        sib.contains(&j.cold) && sib.contains(&j.red),
        "one column per child expected:\n{sib}"
    );
    assert!(
        !sib.contains("REPORT"),
        "REPORT.md edit leaked into siblings:\n{sib}"
    );
    assert!(
        sib.contains("model.py") && sib.contains("depth"),
        "red's code/config deltas missing:\n{sib}"
    );

    r.ok(&["fork", &j.red]);
    assert_eq!(
        r.read("REPORT.md"),
        "# Report\nTried attention.\n",
        "fork moved an off-tree file"
    );
    let before = r.snapshot();
    let o = r.ok(&["apply", &j.cold]);
    let changed: Vec<_> = r
        .snapshot()
        .into_iter()
        .filter(|f| !before.contains(f))
        .map(|f| f.0)
        .collect();
    assert_eq!(
        changed,
        vec!["config.yaml".to_string()],
        "apply should touch only config.yaml:\n{o}"
    );
    assert!(!o.all().to_lowercase().contains("conflict"), "{o}");
    assert!(
        r.read("config.yaml").contains("lr: 1e-4") && r.read("config.yaml").contains("depth: 24")
    );
    // Maya kills it at 41,200 with Ctrl-C (SIGINT to pollard's process group). `run` may exit
    // non-zero, but must still print the node id last.
    let mut c = r.cmd_in(&r.root, &bin(), &["run", "-m", "combine", "train.py"]);
    c.env("KILL_AT", "41200");
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let o = r.exec(c);
    j.blue = o.last_line();
    assert!(is_node_id(&j.blue), "{o}");
    let o = r.ok(&["prune", &j.cold]);
    assert_eq!(o.last_line(), j.cold);
    let t = r.ok(&["tree"]).stdout;
    assert!(
        !t.contains(&j.cold),
        "pruned node visible in default tree:\n{t}"
    );
    assert!(
        r.ok(&["tree", "--all"]).stdout.contains(&j.cold),
        "pruned node missing from tree --all"
    );
    assert!(
        r.ok(&["log", &j.cold, "--key", "val_loss"])
            .stdout
            .contains("50000"),
        "pruned metrics must stay queryable"
    );
    r.ok(&["undo"]);
    assert!(
        !r.ok(&["show", &j.cold]).stdout.contains("pruned"),
        "undo did not bring cold back"
    );
    r.ok(&["prune", &j.cold]);
    j
}

/// Journey A transcript: `init --from-git` prints a summary line (`root <id> (git …) code ✓ …
/// offtree: REPORT.md`). Kept separate so the rest of the journey isn't blocked on output wording.
#[test]
fn journey_a_init_summary_lists_offtree_files() {
    let r = Repo::bare();
    r.write("train.py", "print(1)\n");
    r.write("REPORT.md", "# Report\n");
    git_init_commit(&r);
    let o = r.ok(&["init", "--from-git"]);
    assert!(
        o.all().contains("REPORT.md"),
        "init summary should list the off-tree REPORT.md:\n{o}"
    );
}

#[test]
fn journey_a_first_day() {
    journey_a();
}

#[test]
fn journey_b_exploration_loop() {
    journey_b();
}

/// Journey C: a crash mid-run (blue is Ctrl-C'd at step 41,200 inside journey_b).
fn journey_c() -> J {
    let mut j = journey_b();
    let r = &j.r;
    let s = r.ok(&["show", &j.blue]).stdout;
    assert!(s.contains("killed"), "blue should be `killed`:\n{s}");
    assert!(
        s.contains("41200") || s.contains("41,200") || s.contains("41.2k"),
        "last step 41,200 missing:\n{s}"
    );
    for k in ["10000", "20000", "30000", "40000"] {
        let short = format!("{}k", &k[..2]);
        assert!(
            s.contains(k) || s.contains(&short),
            "checkpoint {k} missing from show:\n{s}"
        );
    }
    assert!(
        s.contains(&format!("pollard fork {} && uv sync --frozen", j.blue)),
        "reproduce line missing:\n{s}"
    );

    let o = r.ok(&["fork", &j.blue, "--step", "30000"]);
    assert!(
        r.root.join("ckpt/step30000.pt").exists(),
        "ckpt/step30000.pt not restored:\n{o}"
    );
    assert!(
        o.all().contains("30000"),
        "fork output should report fork_step=30000:\n{o}"
    );
    r.write(
        "config.yaml",
        r.read("config.yaml").replace("lr: 1e-4", "lr: 5e-5"),
    );
    j.gold = run_train(r, &["-m", "resume from 30k, lower lr"], &[]);
    let pts: Vec<(i64, f64)> = r
        .ok(&["log", &j.gold, "--key", "val_loss"])
        .stdout
        .lines()
        .filter_map(|l| {
            let n = numbers(l);
            (n.len() >= 2 && n[0] > 0.0 && n[0].fract() == 0.0).then(|| (n[0] as i64, n[1]))
        })
        .collect();
    let steps: Vec<i64> = pts.iter().map(|p| p.0).collect();
    assert_eq!(
        steps,
        (1..=500).map(|i| i * 100).collect::<Vec<_>>(),
        "gold's log is not one continuous 100..50000 curve"
    );
    let t = r.ok(&["tree"]).stdout;
    assert!(
        line_with(&t, &j.gold).is_some_and(|l| l.contains("@30000")),
        "tree should label gold's edge @30000:\n{t}"
    );
    let o = r.ok(&["fork", &j.gold, "--step", "20000"]);
    assert!(
        o.all().contains(&j.blue),
        "forking gold at 20k should re-parent to blue with a notice:\n{o}"
    );
    r.ok(&["fork", &j.gold]);
    j
}

#[test]
fn journey_c_crash_mid_run() {
    journey_c();
}

/// Journey D: sweep, pin, share, publish.
#[test]
fn journey_d_sweep_pin_share_publish() {
    let j = journey_c();
    let r = &j.r;
    let remote = tempfile::tempdir().unwrap();
    r.set_config(
        "remote",
        &format!("{:?}", remote.path().display().to_string()),
    );
    let mut members = vec![];
    for s in 1..=16 {
        // seed is written into config.yaml too: the command line alone is not part of the recipe (Q8).
        r.write("config.yaml", format!("lr: 5e-5\ndepth: 24\nseed: {s}\n"));
        let seed_arg = format!("seed={s}");
        let o = po_env(
            r,
            &[
                "run",
                "--sweep",
                "seeds",
                "-m",
                &format!("seed {s}"),
                "--parent",
                &j.gold,
                "train.py",
                &seed_arg,
            ],
            &[],
        );
        assert!(o.ok(), "{o}");
        members.push(o.last_line());
    }
    let t = r.ok(&["tree", "--metric", "val_loss"]).stdout;
    assert!(
        members.iter().all(|m| !t.contains(m.as_str())),
        "sweep members listed individually:\n{t}"
    );
    let sweep_rows: Vec<_> = t.lines().filter(|l| l.contains("seeds")).collect();
    assert_eq!(sweep_rows.len(), 1, "sweep should be one tree row:\n{t}");
    assert!(
        sweep_rows[0].contains("16") && sweep_rows[0].contains('±'),
        "sweep row lacks `16 runs` and mean ± std:\n{t}"
    );
    let sib = r.ok(&["siblings", &j.gold, "--metric", "val_loss"]).stdout;
    assert!(
        sib.contains('±') && members.iter().all(|m| !sib.contains(m.as_str())),
        "sweep not one sibling column:\n{sib}"
    );

    let o = r.ok(&["pin", &j.gold, "paper-v1"]);
    assert_eq!(o.last_line(), j.gold);
    r.ok(&["push"]);
    let snap = listing(remote.path());
    r.ok(&["push"]);
    assert_eq!(listing(remote.path()), snap, "second push transferred data");

    let head = git(r, "rev-parse HEAD");
    r.write("REPORT.md", "# Report\nFINAL\n");
    r.ok(&[
        "export",
        "--path",
        &format!("{}..paper-v1", j.root),
        "--branch",
        "paper-v1",
    ]);
    let revs = git(r, "rev-list paper-v1 ^main");
    let n = if revs.lines().count() == 5 {
        5
    } else {
        git(r, "rev-list paper-v1").lines().count()
    };
    assert_eq!(
        n, 5,
        "expected a 5-commit linear branch (root, warm, red, blue, gold)"
    );
    assert_eq!(git(r, "rev-list --min-parents=2 --count paper-v1"), "0");
    for rev in git(r, "rev-list -n 5 paper-v1").lines() {
        assert!(
            git(r, &format!("show {rev}:REPORT.md")).contains("FINAL"),
            "commit {rev} lacks the current REPORT.md"
        );
        git(r, &format!("show {rev}:.pollard-recipe.json"));
    }
    assert!(
        git(r, "log -1 --format=%s paper-v1").contains(&j.gold),
        "tip commit subject lacks gold's id"
    );
    assert_eq!(git(r, "rev-parse HEAD"), head, "HEAD moved");
    let _ = (&j.warm, &j.red, &j.blue, &j.cold);

    // Collaborator: checkout paper-v1 elsewhere, uv sync --frozen, pollard init --from-git.
    let collab = Repo::bare();
    collab.sh(&format!(
        "git clone -q --branch paper-v1 {} . && uv sync --frozen --quiet",
        r.root.display()
    ));
    let o = collab.ok(&["init", "--from-git"]);
    assert!(is_node_id(&o.last_line()), "{o}");
    assert!(
        collab.read("config.yaml").contains("lr: 5e-5"),
        "collaborator does not have gold's config"
    );
}

fn listing(dir: &std::path::Path) -> Vec<(String, u64)> {
    let mut v = vec![];
    fn go(d: &std::path::Path, v: &mut Vec<(String, u64)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                go(&p, v)
            } else {
                v.push((p.display().to_string(), p.metadata().unwrap().len()))
            }
        }
    }
    go(dir, &mut v);
    v.sort();
    v
}
