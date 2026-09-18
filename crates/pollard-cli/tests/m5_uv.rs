//! M5: uv integration — env hash from `uv.lock`, `uv run` default launch, lock check, `fork` sync.
//! Needs `uv` on PATH and a Python ≥ 3.10. Dependency tests use `six` from PyPI (network).
mod common;
use common::*;

const PYPROJECT: &str = "[project]\nname = \"exp\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = []\n\n[tool.uv]\npackage = false\n";

/// A uv project: pyproject.toml + uv.lock + train.py that reports its interpreter prefix.
fn uv_repo() -> Repo {
    let r = Repo::bare();
    r.write("pyproject.toml", PYPROJECT);
    r.write("config.yaml", "lr: 3\n");
    r.write("train.py", "import sys, os\nopen(os.environ['OUT'], 'w').write(sys.prefix)\n");
    r.sh("uv lock --quiet");
    assert!(r.root.join("uv.lock").exists());
    r.ok(&["init"]);
    r
}

fn run_py(r: &Repo, flags: &[&str]) -> (crate::common::Out, String) {
    let out = r.aux.join(format!("prefix{}.txt", flags.len()));
    let mut args = vec!["run"];
    args.extend_from_slice(flags);
    args.push("train.py");
    let mut c = r.cmd_in(&r.root, &bin(), &args);
    c.env("OUT", &out);
    let o = r.exec(c);
    (o, std::fs::read_to_string(&out).unwrap_or_default())
}

/// §9 M5: "In a uv project, `pollard run train.py` runs under `uv run`".
#[test]
fn run_py_file_uses_uv_run_in_uv_project() {
    let r = uv_repo();
    let (o, prefix) = run_py(&r, &["-m", "a"]);
    assert!(o.ok(), "{o}");
    let venv = r.root.join(".venv").canonicalize().unwrap_or(r.root.join(".venv"));
    assert!(std::path::Path::new(prefix.trim()).canonicalize().ok() == Some(venv.clone()),
        "train.py ran with sys.prefix={prefix:?}, expected the project venv {venv:?} (i.e. via `uv run`)\n{o}");
    assert!(r.ok(&["show", &o.last_line()]).stdout.contains("uv run"), "recorded command should be `uv run train.py`");
}

#[test]
fn run_py_file_without_uv_project_uses_python() {
    let r = Repo::init();
    r.write("train.py", "import sys, os\nopen(os.environ['OUT'], 'w').write(sys.prefix)\n");
    let (o, prefix) = run_py(&r, &["-m", "a"]);
    assert!(o.ok(), "{o}");
    assert!(!prefix.is_empty(), "train.py did not run under plain python:\n{o}");
    assert!(!r.root.join(".venv").exists(), "plain python launch should not create a venv");
}

#[test]
fn explicit_uv_run_accepted_verbatim() {
    let r = uv_repo();
    let out = r.aux.join("v.txt");
    let mut c = r.cmd_in(&r.root, &bin(), &["run", "-m", "a", "--", "uv", "run", "train.py"]);
    c.env("OUT", &out);
    let o = r.exec(c);
    assert!(o.ok() && out.exists(), "{o}");
}

/// §9 M5: "env hash changes when `uv.lock` changes".
#[test]
fn env_hash_changes_when_uv_lock_changes() {
    let r = uv_repo();
    let (o, _) = run_py(&r, &["-m", "a"]);
    let a = o.last_line();
    assert!(o.ok(), "{o}");
    // Change only the lock (requires-python bump + relock): must not be a duplicate recipe.
    r.write("pyproject.toml", PYPROJECT.replace(">=3.10", ">=3.9"));
    r.sh("uv lock --quiet");
    let (o, _) = run_py(&r, &["-m", "b"]);
    assert!(o.ok(), "a changed uv.lock was treated as a duplicate recipe:\n{o}");
    let b = o.last_line();
    let env_line = |id: &str| {
        let s = r.ok(&["show", id]).stdout;
        s.lines().find(|l| l.trim_start().starts_with("env")).map(String::from).unwrap_or_else(|| panic!("no env line in show:\n{s}"))
    };
    assert_ne!(env_line(&a), env_line(&b), "env hash identical across uv.lock change");
}

/// §9 M5: "a stale lock warns, and refuses with `--strict`".
#[test]
fn stale_lock_warns_and_strict_refuses() {
    let r = uv_repo();
    r.write("pyproject.toml", PYPROJECT.replace(">=3.10", ">=3.8")); // lock now stale
    let (o, _) = run_py(&r, &["-m", "strict", "--strict"]);
    assert!(!o.ok(), "--strict accepted a stale uv.lock:\n{o}");
    assert!(ids_in(&r.ok(&["tree"]).stdout).is_empty(), "--strict refusal still created a node");
    let (o, _) = run_py(&r, &["-m", "lenient"]);
    assert!(o.ok(), "{o}");
    assert!(o.stderr.to_lowercase().contains("lock"), "no warning about the stale lock:\n{o}");
}

/// §9 M5: "`fork` leaves the venv matching the node".
#[test]
fn fork_syncs_venv_to_node() {
    let r = uv_repo();
    let (o, _) = run_py(&r, &["-m", "no deps"]);
    let a = o.last_line();
    assert!(o.ok(), "{o}");
    r.sh("uv add --quiet six");
    let (o, _) = run_py(&r, &["-m", "with six"]);
    let b = o.last_line();
    assert!(o.ok(), "{o}");
    let has_six = || r.exec(r.cmd_in(&r.root, std::path::Path::new(".venv/bin/python"), &["-c", "import six"])).ok();
    assert!(has_six());
    r.ok(&["fork", &a]);
    assert!(!r.read("uv.lock").contains("name = \"six\""), "uv.lock not restored by fork");
    assert!(!has_six(), "venv still has `six` after fork to a node without it (uv sync --frozen not run?)");
    r.ok(&["fork", &b]);
    assert!(has_six(), "venv lacks `six` after fork to a node with it");
    // --no-sync leaves the venv alone.
    r.ok(&["fork", &a, "--no-sync"]);
    assert!(has_six(), "--no-sync still synced the venv");
}

/// §4: "`pollard show` prints `pollard fork <node> && uv sync --frozen`".
#[test]
fn show_prints_reproduce_one_liner() {
    let r = uv_repo();
    let (o, _) = run_py(&r, &["-m", "a"]);
    let a = o.last_line();
    let s = r.ok(&["show", &a]).stdout;
    assert!(s.contains(&format!("pollard fork {a} && uv sync --frozen")), "reproduce one-liner missing:\n{s}");
}

#[test]
fn lock_ok_recorded() {
    let r = uv_repo();
    let (o, _) = run_py(&r, &["-m", "a"]);
    let s = r.ok(&["show", &o.last_line()]).stdout.to_lowercase();
    assert!(s.contains("lock"), "lock_ok not shown:\n{s}");
}

#[test]
fn env_hash_without_uv_lock_falls_back() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    let s = r.ok(&["show", &a]).stdout;
    assert!(s.lines().any(|l| l.trim_start().starts_with("env")), "env hash missing when no uv.lock:\n{s}");
}
