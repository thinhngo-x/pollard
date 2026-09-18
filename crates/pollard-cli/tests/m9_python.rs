//! M9: pure-Python SDK, config capture adapters, wheel bundling the binary. SPEC §4 SDK, §8.
//! PyTorch is replaced by a plain Python script (no GPU in CI); the "3 SDK lines vs 10 lines of
//! file I/O" equivalence is tested on metrics + checkpoint registration.
mod common;
use common::*;
use std::path::{Path, PathBuf};

fn python_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python").canonicalize().expect("python/ package dir missing")
}

/// Directory to put on PYTHONPATH so `import pollard` finds the in-repo SDK.
fn sdk_path() -> PathBuf {
    let p = python_dir();
    if p.join("src/pollard").is_dir() { p.join("src") } else { p }
}

fn run_py(r: &Repo, note: &str, code: &str, extra_env: &[(&str, &str)]) -> Out {
    let n = r.aux.join(format!("{}.py", note.replace(' ', "_")));
    std::fs::write(&n, code).unwrap();
    let mut c = r.cmd_in(&r.root, &bin(), &["run", "-m", note, "--force", "--", "python3", n.to_str().unwrap()]);
    c.env("PYTHONPATH", sdk_path());
    for (k, v) in extra_env { c.env(k, v); }
    r.exec(c)
}

const SDK_SCRIPT: &str = r#"
import os
src = os.environ["SRC_CKPT"]
import pollard                                  # SDK line 1
run = pollard.current()                         # SDK line 2
for step in (10, 20, 30):
    loss = 1.0 / step
    run.log({"loss": loss}, step=step)          # SDK line 3
run.save_checkpoint(src)
"#;

const PLAIN_SCRIPT: &str = r#"
import os, shutil, json
src = os.environ["SRC_CKPT"]
metrics = open(os.environ["POLLARD_METRICS"], "a")
ckpt_dir = os.environ["POLLARD_CKPT_DIR"]
for step in (10, 20, 30):
    loss = 1.0 / step
    metrics.write(json.dumps({"step": step, "loss": loss}) + "\n")
    metrics.flush()
shutil.copy(src, os.path.join(ckpt_dir, "w.pt"))
"#;

/// §9 M9: "a script logs metrics and a checkpoint with 3 added SDK lines; the same script works
/// with the SDK removed and 10 lines of plain file I/O instead".
#[test]
fn sdk_and_plain_file_io_are_equivalent() {
    let r = Repo::init();
    let src = r.aux.join("src.pt");
    std::fs::write(&src, prng_bytes(2 * MB, 3)).unwrap();
    let env = [("SRC_CKPT", src.to_str().unwrap())];
    let o_sdk = run_py(&r, "sdk", SDK_SCRIPT, &env);
    assert!(o_sdk.ok(), "{o_sdk}");
    let o_plain = run_py(&r, "plain", PLAIN_SCRIPT, &env);
    assert!(o_plain.ok(), "{o_plain}");
    let (a, b) = (o_sdk.last_line(), o_plain.last_line());
    let la = r.ok(&["log", &a, "--key", "loss"]).stdout;
    let lb = r.ok(&["log", &b, "--key", "loss"]).stdout;
    for step in [10.0, 20.0, 30.0] {
        assert!(has_num(&la, step) && has_num(&la, 1.0 / step), "SDK run missing step {step}:\n{la}");
        assert!(has_num(&lb, step) && has_num(&lb, 1.0 / step), "plain run missing step {step}:\n{lb}");
    }
    let sa = r.ok(&["show", &a]).stdout;
    let sb = r.ok(&["show", &b]).stdout;
    assert!(sa.contains("src.pt"), "SDK save_checkpoint not registered:\n{sa}");
    assert!(sb.contains("w.pt"), "plain-I/O checkpoint not registered:\n{sb}");
}

#[test]
fn current_raises_outside_pollard_run() {
    let r = Repo::bare();
    let mut c = r.cmd_in(&r.root, Path::new("python3"), &["-c", "import pollard\ntry:\n    pollard.current()\nexcept Exception as e:\n    print('RAISED', type(e).__name__)\n"]);
    c.env("PYTHONPATH", sdk_path());
    let o = r.exec(c);
    assert!(o.stdout.contains("RAISED"), "pollard.current() did not raise outside `pollard run`:\n{o}");
}

#[test]
fn fork_step_helper() {
    let r = Repo::init();
    let res = r.aux.join("fs.txt");
    let code = format!("import pollard\nopen({:?}, 'w').write(repr(pollard.fork_step()))\nr = pollard.current()\nr.log({{'loss': 1.0}}, step=5)\n", res.display().to_string());
    let o = run_py(&r, "a", &code, &[]);
    assert!(o.ok(), "{o}");
    assert_eq!(std::fs::read_to_string(&res).unwrap(), "None");
    let a = o.last_line();
    r.ok(&["fork", &a, "--step", "5", "--no-sync"]);
    let o = run_py(&r, "b", &code, &[]);
    assert!(o.ok(), "{o}");
    assert_eq!(std::fs::read_to_string(&res).unwrap(), "5");
}

#[test]
fn set_config_in_sdk_mode_is_captured() {
    let r = Repo::bare();
    r.write("train.txt", "x\n");
    r.ok(&["init"]); // no config.yaml → sdk capture mode by default
    let a = run_py(&r, "c1", "import pollard\nr = pollard.current()\nr.set_config({'lr': 0.1, 'depth': 12})\nr.log({'loss': 1.0}, step=1)\n", &[]);
    let b = run_py(&r, "c2", "import pollard\nr = pollard.current()\nr.set_config({'lr': 0.2, 'depth': 12})\nr.log({'loss': 1.0}, step=1)\n", &[]);
    assert!(a.ok() && b.ok(), "{a}\n{b}");
    let d = r.ok(&["diff", &a.last_line(), &b.last_line()]).stdout;
    assert!(d.contains("lr") && d.contains("0.1") && d.contains("0.2"), "set_config not captured:\n{d}");
}

#[test]
fn siblings_helper_returns_table() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 1 loss=2");
    r.write("config.yaml", "lr: 1\n");
    let c = r.run(&["-m", "c", "--parent", &p], "emit 1 loss=1");
    let mut cmd = r.cmd_in(&r.root, Path::new("python3"), &["-c", &format!("import pollard\nt = pollard.siblings({p:?})\nprint(t)\n")]);
    cmd.env("PYTHONPATH", sdk_path());
    let o = r.exec(cmd);
    assert!(o.ok() && o.stdout.contains(&c), "pollard.siblings() failed or lacks child {c}:\n{o}");
}

#[test]
fn sdk_core_is_pure_python_under_200_lines() {
    let dir = sdk_path().join("pollard");
    let mut core_lines = 0;
    for e in std::fs::read_dir(&dir).expect("pollard package dir").flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        assert!(!matches!(ext, "so" | "pyd" | "c" | "pyx"), "compiled extension in SDK: {}", p.display());
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if ext == "py" && !name.contains("wandb") && !name.contains("neptune") && !name.contains("forward") {
            core_lines += std::fs::read_to_string(&p).unwrap().lines().count();
        }
    }
    assert!(core_lines < 200, "SDK core is {core_lines} lines (target < 200, §4)");
}

/// §9 M9: "`uv tool install` from a built wheel" — the wheel bundles the Rust binary.
#[test]
fn uv_tool_install_from_built_wheel() {
    let tmp = tempfile::tempdir().unwrap();
    let dist = tmp.path().join("dist");
    let r = Repo::bare();
    let o = r.exec(r.cmd_in(&python_dir(), Path::new("uv"), &["build", "--wheel", "-o", dist.to_str().unwrap()]));
    assert!(o.ok(), "uv build --wheel failed:\n{o}");
    let wheel = std::fs::read_dir(&dist).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "whl")).expect("no wheel built");
    let (tool_dir, bin_dir) = (tmp.path().join("tools"), tmp.path().join("bin"));
    let mut c = r.cmd_in(&r.root, Path::new("uv"), &["tool", "install", wheel.to_str().unwrap()]);
    c.env("UV_TOOL_DIR", &tool_dir).env("UV_TOOL_BIN_DIR", &bin_dir);
    let o = r.exec(c);
    assert!(o.ok(), "uv tool install failed:\n{o}");
    let installed = bin_dir.join("pollard");
    assert!(installed.exists(), "`pollard` not on the tool bin dir after install:\n{o}");
    let o = r.exec(r.cmd_in(&r.root, &installed, &["init"]));
    assert!(o.ok() && r.root.join(".pollard").is_dir(), "installed pollard binary does not work:\n{o}");
}
