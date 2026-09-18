//! uv integration (§4): launch rule, lock check, env hash, env delta, `uv sync`.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::delta::EnvChange;
use crate::{Repo, Result, msg};

/// Inputs to the env hash. `env` = blake3 of this record's JSON, stored as an object.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EnvInputs {
    /// uv.lock contents, if present
    pub lock: Option<String>,
    /// `uv pip freeze` output when there is no uv.lock
    pub freeze: Option<String>,
    pub python_version: Option<String>,
    pub requires_python: Option<String>,
    pub cuda: String,
    /// PEP 723 `# /// script` block of the launched script
    pub pep723: Option<String>,
}

fn is_uv_project(root: &Path) -> bool {
    root.join("pyproject.toml").is_file() || root.join("uv.lock").is_file()
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// `train.py args` → `uv run train.py args` in a uv project, else `python train.py args`.
pub fn launch_command(root: &Path, cmd: &[String]) -> Vec<String> {
    match cmd.first() {
        Some(first) if first.ends_with(".py") => {
            let pre: &[&str] = if is_uv_project(root) {
                &["uv", "run"]
            } else if on_path("python") {
                &["python"]
            } else {
                &["python3"]
            };
            pre.iter().map(|s| s.to_string()).chain(cmd.iter().cloned()).collect()
        }
        _ => cmd.to_vec(),
    }
}

/// `uv lock --check` when a `uv.lock` exists; None when not applicable or uv is missing.
pub fn lock_check(root: &Path) -> Option<bool> {
    if !root.join("uv.lock").is_file() {
        return None;
    }
    let out = Command::new("uv")
        .args(["lock", "--check", "--offline", "--quiet"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some(out.status.success())
}

/// CUDA/driver string; `POLLARD_CUDA` overrides (tests, clusters without nvidia-smi on PATH).
fn cuda_string() -> String {
    if let Ok(s) = std::env::var("POLLARD_CUDA") {
        return s;
    }
    if !on_path("nvidia-smi") {
        return "none".into();
    }
    Command::new("nvidia-smi")
        .args(["--query-gpu=name,driver_version", "--format=csv,noheader"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "none".into())
}

fn pep723(root: &Path, cmd: &[String]) -> Option<String> {
    let script = cmd.iter().find(|a| a.ends_with(".py"))?;
    let text = std::fs::read_to_string(root.join(script)).ok()?;
    let start = text.find("# /// script")?;
    let end = text[start..].find("\n# ///")? + start;
    Some(text[start..end].to_string())
}

pub fn inputs(root: &Path, cmd: &[String]) -> EnvInputs {
    let read = |p: &str| std::fs::read_to_string(root.join(p)).ok();
    let lock = read("uv.lock");
    let freeze = if lock.is_none() && on_path("uv") {
        Command::new("uv")
            .args(["pip", "freeze"])
            .current_dir(root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    };
    let requires_python = read("pyproject.toml").and_then(|t| {
        let v: toml::Value = toml::from_str(&t).ok()?;
        Some(v.get("project")?.get("requires-python")?.as_str()?.to_string())
    });
    EnvInputs {
        lock,
        freeze,
        python_version: read(".python-version").map(|s| s.trim().to_string()),
        requires_python,
        cuda: cuda_string(),
        pep723: pep723(root, cmd),
    }
}

pub fn store(repo: &Repo, e: &EnvInputs) -> Result<String> {
    Ok(repo.objects.put_object(&serde_json::to_vec(e)?)?)
}

fn load(repo: &Repo, hash: &str) -> Option<EnvInputs> {
    serde_json::from_slice(&repo.objects.get_object(hash).ok()?).ok()
}

/// Package name → version from uv.lock (`[[package]]`) or `pip freeze` lines.
fn packages(e: &EnvInputs) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(lock) = &e.lock {
        if let Ok(v) = toml::from_str::<toml::Value>(lock) {
            for p in v.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
                if let (Some(n), Some(ver)) = (p.get("name").and_then(|x| x.as_str()), p.get("version").and_then(|x| x.as_str())) {
                    out.insert(n.to_string(), ver.to_string());
                }
            }
        }
    } else if let Some(f) = &e.freeze {
        for line in f.lines() {
            if let Some((n, v)) = line.split_once("==") {
                out.insert(n.trim().to_string(), v.trim().to_string());
            }
        }
    }
    let mut extra = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v.clone());
        }
    };
    extra("python", &e.python_version);
    extra("requires-python", &e.requires_python);
    extra("cuda", &Some(e.cuda.clone()));
    out
}

/// Key diff of the parsed lockfile plus version strings.
pub fn delta(repo: &Repo, old: &str, new: &str) -> Vec<EnvChange> {
    if old == new {
        return vec![];
    }
    let (Some(a), Some(b)) = (load(repo, old), load(repo, new)) else { return vec![] };
    let (pa, pb) = (packages(&a), packages(&b));
    let keys: std::collections::BTreeSet<&String> = pa.keys().chain(pb.keys()).collect();
    keys.into_iter()
        .filter(|k| pa.get(*k) != pb.get(*k))
        .map(|k| EnvChange { name: k.clone(), old: pa.get(k).cloned(), new: pb.get(k).cloned() })
        .collect()
}

/// `uv sync --frozen` in a uv project; no-op elsewhere.
pub fn sync(root: &Path) -> Result<bool> {
    if !root.join("uv.lock").is_file() {
        return Ok(false);
    }
    let st = Command::new("uv")
        .args(["sync", "--frozen"])
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| msg(format!("uv sync --frozen: {e}")))?;
    if !st.success() {
        return Err(msg(format!("uv sync --frozen failed in {}", root.display())));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn launch_rule() {
        let t = tempfile::tempdir().unwrap();
        let cmd = vec!["train.py".to_string(), "seed=1".into()];
        let plain = super::launch_command(t.path(), &cmd);
        assert!(plain[0].starts_with("python") && plain[1] == "train.py");
        std::fs::write(t.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert_eq!(super::launch_command(t.path(), &cmd), ["uv", "run", "train.py", "seed=1"]);
        let verbatim = vec!["uv".to_string(), "run".into(), "train.py".into()];
        assert_eq!(super::launch_command(t.path(), &verbatim), verbatim);
    }
}
