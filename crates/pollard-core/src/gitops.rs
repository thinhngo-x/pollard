//! Git import/export glue (§7). Owner: senior dev. The git work itself is in `pollard-git`.

use std::collections::BTreeMap;

use pollard_objects::{Entry, MODE_FILE, Manifest};
use serde_json::{Value, json};

use crate::node::{self, Node, Status};
use crate::wc::{self, Capture};
use crate::{OpRecord, Repo, Result, delta, env, metrics, msg, ops};

fn git_err(e: pollard_git::Error) -> crate::Error {
    msg(e.to_string())
}

/// `pollard import <git-rev>` (and `init --from-git` = `import HEAD`): create a root node
/// whose `code` is the git tree hash of the commit's files after the code-manifest rules
/// (§7 v3); config, data, env and docs come from the current working copy. The working
/// copy is not touched. The root is `done`.
pub fn import(repo: &mut Repo, rev: &str) -> Result<OpRecord> {
    let tmp = repo.dot.join(format!("tmp/import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let snap = pollard_git::checkout_to(&repo.root, rev, &tmp)
        .map_err(git_err)
        .and_then(|commit| {
            Ok((
                commit,
                repo.objects.snapshot_dir(&tmp, &wc::code_opts(repo))?,
            ))
        });
    let _ = std::fs::remove_dir_all(&tmp);
    let (commit, snap) = snap?;
    let code = wc::code_hash(repo, &snap.manifest)?;

    let cfg = match wc::capture_mode(repo) {
        Capture::File(p) if repo.root.join(&p).is_file() => {
            wc::read_config_file(&repo.root.join(p))?
        }
        _ => Value::Object(Default::default()),
    };
    let config = wc::store_config(repo, &cfg)?;
    let data = wc::data_manifest(repo)?;
    let env_hash = env::store(repo, &env::inputs(&repo.root, &[]))?;
    let docs = repo
        .objects
        .snapshot_dir(&repo.root, &wc::docs_opts(repo))?
        .manifest;
    let id = repo.next_id()?;
    let n = Node {
        id: id.clone(),
        parent: None,
        recipe_hash: node::recipe_hash(&code, &config, &data, &env_hash),
        code,
        config,
        data,
        env: env_hash,
        weights: None,
        docs: (!docs.entries.is_empty()).then(|| docs.hash()),
        fork_step: None,
        status: Status::Done,
        created_at: crate::now(),
        finished_at: Some(crate::now()),
        command: format!("pollard import {rev}"),
        note: Some(format!("import {rev} (git {})", &commit[..7])),
        note_auto: false,
        lock_ok: None,
        depth: 0,
        sweep: None,
    };
    ops::record(repo, None, |repo| {
        n.insert(&repo.db)?;
        repo.set_head(Some(&n.id))?;
        Ok(n.id.clone())
    })
}

/// Result of `export`: the branch written and `(node, commit)` pairs, oldest first.
pub struct Exported {
    pub branch: String,
    pub commits: Vec<(String, String)>,
}

/// `pollard export <node>` or `--path a..b` (`spec` is either form): one commit per node,
/// linear, the first on top of HEAD. Each tree = the node's code tree + the current working
/// copy's off-tree files (§7 v3) + `.pollard-recipe.json`. Writes only objects and
/// `refs/heads/<branch>` (default `pollard/<last node>`); HEAD and the index are untouched.
pub fn export(repo: &Repo, spec: &str, branch: Option<&str>) -> Result<Exported> {
    let chain = match spec.split_once("..") {
        Some((a, b)) => {
            let (a, b) = (repo.resolve(a)?, repo.resolve(b)?);
            let mut up = node::ancestry(&repo.db, &b)?;
            let Some(i) = up.iter().position(|n| n.id == a) else {
                return Err(msg(format!("export --path: {a} is not an ancestor of {b}")));
            };
            up.truncate(i + 1);
            up.reverse();
            up
        }
        None => vec![repo.node(&repo.resolve(spec)?)?],
    };
    let docs = repo
        .objects
        .snapshot_dir(&repo.root, &wc::docs_opts(repo))?
        .manifest;
    let mut commits = Vec::new();
    for n in &chain {
        let code = repo
            .objects
            .get_manifest(&wc::manifest_of(repo, &n.code)?)?;
        let mut files: BTreeMap<String, Entry> = code
            .entries
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        files.extend(docs.entries.iter().map(|e| (e.path.clone(), e.clone())));
        let recipe = serde_json::to_vec_pretty(&recipe_json(repo, n)?)?;
        let (hash, size) = repo.objects.put_bytes(&recipe)?;
        let path = ".pollard-recipe.json".to_string();
        files.insert(
            path.clone(),
            Entry {
                path,
                size,
                hash,
                mode: MODE_FILE,
            },
        );
        commits.push((
            Manifest::new(files.into_values().collect()),
            message(repo, n)?,
        ));
    }
    let last = &chain.last().expect("chain is never empty").id;
    let branch = branch.map_or_else(|| format!("pollard/{last}"), str::to_string);
    let ids = pollard_git::export(&repo.root, &repo.objects, &commits, &branch).map_err(git_err)?;
    Ok(Exported {
        branch,
        commits: chain.iter().map(|n| n.id.clone()).zip(ids).collect(),
    })
}

fn recipe_json(repo: &Repo, n: &Node) -> Result<Value> {
    let data: Vec<Value> = repo
        .objects
        .get_manifest(&n.data)?
        .entries
        .into_iter()
        .map(|e| json!({"path": e.path, "size": e.size, "blake3": e.hash}))
        .collect();
    Ok(json!({
        "node": n.id,
        "parent": n.parent,
        "fork_step": n.fork_step,
        "code": n.code,
        "config": wc::load_config(repo, &n.config)?,
        "config_hash": n.config,
        "data": data,
        "data_hash": n.data,
        "env": n.env,
        "recipe_hash": n.recipe_hash,
        "command": n.command,
    }))
}

/// First line: id, pins, note title. Body: config delta from the parent and the primary
/// metric at `last_own` (§7).
fn message(repo: &Repo, n: &Node) -> Result<String> {
    let mut first = n.id.clone();
    let pins = repo.pins_of(&n.id)?;
    if !pins.is_empty() {
        first.push_str(&format!(" ({})", pins.join(", ")));
    }
    if !n.title().is_empty() {
        first.push_str(&format!(": {}", n.title()));
    }
    let mut body: Vec<String> = delta::load(repo, &n.id)?
        .config
        .iter()
        .map(delta::fmt_change)
        .collect();
    let key = match &repo.config.primary_metric {
        Some(k) => Some(k.clone()),
        None => metrics::keys(&repo.db, &n.id)?.into_iter().next(),
    };
    if let Some(k) = key
        && let Some((step, v)) = metrics::series(&repo.db, &n.id, &k)?.last()
    {
        body.push(format!("{k} = {v} at step {step}"));
    }
    Ok(if body.is_empty() {
        first
    } else {
        format!("{first}\n\n{}", body.join("\n"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn import_then_export() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::write(r.join("train.py"), "print(1)").unwrap();
        fs::write(r.join("config.yaml"), "lr: 0.1\n").unwrap();
        fs::write(r.join("REPORT.md"), "report").unwrap();
        fs::write(r.join(".gitignore"), ".pollard/\n").unwrap();
        git(r, &["init", "-q"]);
        git(r, &["add", "-A"]);
        git(r, &["commit", "-qm", "init"]);
        let head = git(r, &["rev-parse", "HEAD"]);
        let head_tree = git(r, &["rev-parse", "HEAD^{tree}"]);

        let mut repo = Repo::init(r).unwrap();
        let rec = import(&mut repo, "HEAD").unwrap();
        let root = repo.node(&rec.node).unwrap();
        assert_eq!(repo.head().unwrap().as_deref(), Some(root.id.as_str()));
        // REPORT.md is off-tree, so code != HEAD's tree; code = tree without it
        git(r, &["rm", "-q", "--cached", "REPORT.md"]);
        assert_eq!(root.code, git(r, &["write-tree"]));
        git(r, &["reset", "-q"]);

        fs::write(r.join("REPORT.md"), "report v2").unwrap();
        let status = git(r, &["status", "--porcelain"]);
        repo.cmdline = "pin".into();
        crate::cmd::pin(&mut repo, &root.id, "base").unwrap();
        let ex = export(&repo, &root.id, Some("exp")).unwrap();
        assert_eq!(ex.branch, "exp");
        assert_eq!(git(r, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(r, &["status", "--porcelain"]), status);
        // tree minus the recipe file == HEAD's tree, except REPORT.md comes from the working copy
        assert_eq!(git(r, &["show", "exp:REPORT.md"]), "report v2");
        let subject = git(r, &["show", "-s", "--format=%s", "exp"]);
        assert!(
            subject.starts_with(&format!("{} (base): import HEAD", root.id)),
            "{subject}"
        );
        let recipe: Value =
            serde_json::from_str(&git(r, &["show", "exp:.pollard-recipe.json"])).unwrap();
        assert_eq!(recipe["config"], json!({"lr": 0.1}));

        fs::write(r.join("REPORT.md"), "report").unwrap();
        let ex = export(&repo, &root.id, Some("exp2")).unwrap();
        git(r, &["read-tree", "--index-output=.git/tmp-index", "exp2"]);
        let tree = Command::new("git")
            .args(["rm", "-q", "--cached", ".pollard-recipe.json"])
            .env("GIT_INDEX_FILE", r.join(".git/tmp-index"))
            .current_dir(r)
            .status()
            .unwrap();
        assert!(tree.success());
        let out = Command::new("git")
            .arg("write-tree")
            .env("GIT_INDEX_FILE", r.join(".git/tmp-index"))
            .current_dir(r)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap().trim(),
            head_tree,
            "{}",
            ex.commits[0].1
        );
    }
}
