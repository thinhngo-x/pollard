//! Working copy: code/docs/data manifests, config capture, checkout. This is the single
//! place that applies the code-manifest walk rules (PLAN C3).

use std::path::Path;

use pollard_objects::{Entry, Manifest, WalkOptions};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::{IoCtx, Repo, Result, msg};

/// Walk rules for the code manifest: ignore files, 10 MB limit, minus off-tree files
/// (gitignore semantics incl. `!`), output dirs and the checkpoint dir.
pub fn code_opts(repo: &Repo) -> WalkOptions {
    let c = &repo.config;
    let mut ex: Vec<String> = c.output_dirs.iter().map(|g| dir_glob(g)).collect();
    ex.push(dir_glob(&c.checkpoint_dir));
    WalkOptions { offtree: c.offtree.clone(), ..WalkOptions::code(&ex) }
}

/// Anchor bare directory names (`outputs` → `outputs/`) so they match the whole dir.
fn dir_glob(g: &str) -> String {
    let g = g.trim_start_matches("./");
    if !g.contains('*') && !g.ends_with('/') && !g.contains('.') { format!("{g}/") } else { g.to_string() }
}

/// Walk rules for off-tree docs: only files matching `offtree`.
pub fn docs_opts(repo: &Repo) -> WalkOptions {
    WalkOptions { offtree: repo.config.offtree.clone(), offtree_only: true, ignore_files: true, ..Default::default() }
}

/// Git tree hash of a stored code manifest; remembers the mapping in `code_trees`.
pub fn code_hash(repo: &Repo, m: &Manifest) -> Result<String> {
    let mh = m.hash();
    let tree = pollard_git::tree_hash(&repo.objects, m).map_err(|e| msg(format!("git tree hash: {e}")))?;
    repo.db.execute("INSERT OR IGNORE INTO code_trees(git_tree, manifest_hash) VALUES(?1,?2)", [&tree, &mh])?;
    Ok(tree)
}

/// Code manifest hash for a node's `code` (git tree hash).
pub fn manifest_of(repo: &Repo, git_tree: &str) -> Result<String> {
    repo.db
        .query_row("SELECT manifest_hash FROM code_trees WHERE git_tree=?1", [git_tree], |r| r.get(0))
        .optional()?
        .ok_or_else(|| msg(format!("no stored code manifest for tree {git_tree}")))
}

/// Store the current working copy (code scope) and return its manifest hash.
pub fn snapshot_manifest(repo: &Repo) -> Result<String> {
    Ok(repo.objects.snapshot_dir(&repo.root, &code_opts(repo))?.manifest.hash())
}

/// Make the code scope of the working copy match a stored manifest.
pub fn checkout(repo: &Repo, manifest_hash: &str) -> Result<()> {
    let m = repo.objects.get_manifest(manifest_hash)?;
    repo.objects.materialize(&m, &repo.root, &code_opts(repo))?;
    Ok(())
}

/// Resolved config-capture mode.
#[derive(Debug, Clone, PartialEq)]
pub enum Capture {
    File(String),
    Sdk,
    Hydra,
}

/// Path of the captured config file, if capture mode is `file:`.
pub fn config_file(repo: &Repo) -> Option<String> {
    match capture_mode(repo) {
        Capture::File(p) => Some(p.trim_start_matches("./").to_string()),
        _ => None,
    }
}

pub fn capture_mode(repo: &Repo) -> Capture {
    match repo.config.config_capture.as_deref() {
        Some("sdk") => Capture::Sdk,
        Some("hydra") => Capture::Hydra,
        Some(s) if s.starts_with("file:") => Capture::File(s[5..].to_string()),
        _ if repo.root.join("config.yaml").is_file() => Capture::File("config.yaml".into()),
        _ => Capture::Sdk,
    }
}

/// Parse a YAML or JSON config file into JSON.
pub fn read_config_file(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).at(path)?;
    let v: Value = serde_yaml::from_str(&text).map_err(|e| msg(format!("{}: {e}", path.display())))?;
    Ok(if v.is_null() { Value::Object(Default::default()) } else { v })
}

/// Hydra-style `key.path=value` overrides from the launch args (leading `+`/`++` allowed).
pub fn overrides(args: &[String]) -> Vec<(String, Value)> {
    args.iter()
        .filter_map(|a| {
            let (k, v) = a.split_once('=')?;
            let k = k.trim_start_matches('+');
            let ok = !k.is_empty()
                && !k.starts_with('-')
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                && !k.starts_with('.');
            ok.then(|| (k.to_string(), serde_yaml::from_str::<Value>(v).unwrap_or(Value::String(v.into()))))
        })
        .collect()
}

pub fn apply_overrides(cfg: &mut Value, ovs: &[(String, Value)]) {
    for (path, v) in ovs {
        let mut cur = &mut *cfg;
        let parts: Vec<&str> = path.split('.').collect();
        for (i, p) in parts.iter().enumerate() {
            if !cur.is_object() {
                *cur = Value::Object(Default::default());
            }
            let obj = cur.as_object_mut().unwrap();
            if i == parts.len() - 1 {
                obj.insert(p.to_string(), v.clone());
                break;
            }
            cur = obj.entry(p.to_string()).or_insert(Value::Object(Default::default()));
        }
    }
}

/// Store the canonical JSON (sorted keys) of a config; the object hash is the `config` hash.
pub fn store_config(repo: &Repo, cfg: &Value) -> Result<String> {
    Ok(repo.objects.put_object(serde_json::to_string(cfg)?.as_bytes())?)
}

pub fn load_config(repo: &Repo, hash: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&repo.objects.get_object(hash)?)?)
}

/// Data manifest over the local roots in `config.toml`, rehashing only files whose
/// `(mtime, size)` changed. Remote roots contribute one entry keyed by URL.
// ponytail: remote roots are hashed by URL only until the remote listing (D-2) lands.
pub fn data_manifest(repo: &Repo) -> Result<String> {
    let mut entries = vec![];
    for root in &repo.config.data {
        if root.contains("://") {
            entries.push(Entry { path: root.clone(), size: 0, hash: crate::b3(root.as_bytes()), mode: 0 });
            continue;
        }
        let base = repo.root.join(root);
        if !base.exists() {
            eprintln!("warning: data root {} does not exist", base.display());
            continue;
        }
        for ent in ignore::WalkBuilder::new(&base).standard_filters(false).build() {
            let ent = ent.map_err(|e| msg(format!("{}: {e}", base.display())))?;
            let md = ent.metadata().map_err(|e| msg(format!("{}: {e}", ent.path().display())))?;
            if !md.is_file() {
                continue;
            }
            let p = ent.path();
            let rel = p.strip_prefix(&repo.root).unwrap_or(p).to_string_lossy().into_owned();
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as i64);
            let size = md.len() as i64;
            let cached: Option<String> = repo
                .db
                .query_row(
                    "SELECT hash FROM data_cache WHERE path=?1 AND mtime=?2 AND size=?3",
                    params![rel, mtime, size],
                    |r| r.get(0),
                )
                .optional()?;
            let hash = match cached {
                Some(h) => h,
                None => {
                    let h = pollard_objects::hash_file(p)?;
                    repo.db.execute(
                        "INSERT OR REPLACE INTO data_cache(path,mtime,size,hash) VALUES(?1,?2,?3,?4)",
                        params![rel, mtime, size, h],
                    )?;
                    h
                }
            };
            entries.push(Entry { path: rel, size: size as u64, hash, mode: 0o100644 });
        }
    }
    Ok(repo.objects.put_manifest(&Manifest::new(entries))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_apply() {
        let args: Vec<String> = ["train.py", "seed=3", "model.depth=24", "--flag", "+opt.name=adam", "a=b=c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ovs = overrides(&args);
        let mut cfg = json!({"lr": 0.1, "model": {"depth": 12}});
        apply_overrides(&mut cfg, &ovs);
        assert_eq!(cfg, json!({"lr": 0.1, "seed": 3, "model": {"depth": 24}, "opt": {"name": "adam"}, "a": "b=c"}));
    }
}
