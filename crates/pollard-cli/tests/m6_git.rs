//! M6: git tree hashing, `import`, `export` single and `--path`, `init --from-git`. SPEC §7.
mod common;
use common::*;

/// v3 §9 M6: a HEAD with no off-tree, ignored, or over-10 MB files.
fn clean_git_repo() -> Repo {
    let r = Repo::bare();
    r.write("train.py", "print('train')\n");
    r.write("config.yaml", "lr: 3\n");
    r.write("src/model.py", "x = 1\n");
    r.write(".gitignore", ".pollard\n");
    git_init_commit(&r);
    r
}

/// Same plus an off-tree REPORT.md committed to git.
fn git_repo() -> Repo {
    let r = Repo::bare();
    r.write("train.py", "print('train')\n");
    r.write("config.yaml", "lr: 3\n");
    r.write("src/model.py", "x = 1\n");
    r.write("REPORT.md", "# report v1\n");
    r.write(".gitignore", ".pollard\n");
    git_init_commit(&r);
    r
}

/// Tree hash of `rev` with `.pollard-recipe.json` removed (temp index; real index untouched).
fn tree_without_recipe(r: &Repo, rev: &str) -> String {
    let idx = r.aux.join("tmp-index");
    r.sh(&format!(
        "GIT_INDEX_FILE={i} git read-tree {rev} && GIT_INDEX_FILE={i} git rm -q --cached --ignore-unmatch .pollard-recipe.json && GIT_INDEX_FILE={i} git write-tree > {i}.out",
        i = idx.display()
    ));
    std::fs::read_to_string(format!("{}.out", idx.display())).unwrap().trim().to_string()
}

/// §7: `code` is a git tree hash; `init --from-git` == `import HEAD`.
#[test]
fn init_from_git_imports_head_as_root_with_git_tree_hash() {
    let r = clean_git_repo();
    let head_tree = git(&r, "rev-parse HEAD^{tree}");
    let o = r.ok(&["init", "--from-git"]);
    let root = o.last_line();
    assert!(is_node_id(&root), "init --from-git must print the root id last:\n{o}");
    assert!(o.stdout.contains(&head_tree[..7]) || r.ok(&["show", &root]).stdout.contains(&head_tree),
        "root's code hash is not HEAD's tree hash {head_tree}");
    let s = r.ok(&["show", &root]).stdout;
    assert!(s.contains(&head_tree), "show root lacks code = {head_tree}:\n{s}");
}

#[test]
fn import_rev_creates_root_node() {
    let r = clean_git_repo();
    let first_tree = git(&r, "rev-parse HEAD^{tree}");
    r.write("src/model.py", "x = 2\n");
    r.sh("git commit -qam second");
    r.ok(&["init"]);
    let o = r.ok(&["import", "HEAD~1"]);
    let id = o.last_line();
    assert!(is_node_id(&id), "{o}");
    let s = r.ok(&["show", &id]).stdout;
    assert!(s.contains(&first_tree), "import HEAD~1: code != its tree {first_tree}:\n{s}");
}

/// §9 M6 (v3): clean HEAD → `import HEAD` gives `code` == HEAD's tree; `export` of that node gives a
/// commit whose tree minus `.pollard-recipe.json` == HEAD's tree; HEAD untouched.
#[test]
fn import_then_export_tree_equals_head_and_head_untouched() {
    let r = clean_git_repo();
    r.ok(&["init"]);
    let head = git(&r, "rev-parse HEAD");
    let head_tree = git(&r, "rev-parse HEAD^{tree}");
    let status_before = git(&r, "status --porcelain");
    let id = r.ok(&["import", "HEAD"]).last_line();
    let s = r.ok(&["show", &id]).stdout;
    assert!(s.contains(&head_tree), "imported node's code != HEAD's tree {head_tree}:\n{s}");
    let o = r.ok(&["export", &id, "--branch", "exp1"]);
    assert_eq!(tree_without_recipe(&r, "exp1"), head_tree, "exported tree minus .pollard-recipe.json != HEAD's tree\n{o}");
    assert_eq!(git(&r, "rev-parse HEAD"), head, "HEAD moved");
    assert_eq!(git(&r, "symbolic-ref HEAD"), "refs/heads/main", "HEAD switched branch");
    assert_eq!(git(&r, "status --porcelain"), status_before, "index/working tree touched by export");
}

#[test]
fn export_single_run_node_has_generated_message() {
    let r = git_repo();
    r.ok(&["init", "--from-git"]);
    r.write("config.yaml", "lr: 1\n");
    let id = r.run(&["-m", "lower lr"], "emit 10 loss=2.5");
    r.ok(&["export", &id, "--branch", "one"]);
    let msg = git(&r, "log -1 --format=%B one");
    let first = msg.lines().next().unwrap_or("");
    assert!(first.contains(&id) && first.contains("lower lr"), "first line must carry node id and note title:\n{msg}");
    assert!(msg.contains("lr"), "body should carry the config delta:\n{msg}");
    let files = git(&r, "ls-tree -r --name-only one");
    assert!(files.contains("REPORT.md"), "off-tree file missing from export:\n{files}");
    assert!(files.contains("src/model.py") && files.contains("config.yaml"));
}

/// §9 M6: "`export --path` of a 4-node chain yields 4 linear commits with generated messages and
/// `.pollard-recipe.json`; off-tree files are included; HEAD is untouched".
#[test]
fn export_path_four_node_chain() {
    let r = git_repo();
    let root = r.ok(&["init", "--from-git"]).last_line();
    let head = git(&r, "rev-parse HEAD");
    let mut chain = vec![root.clone()];
    for i in 1..=3 {
        r.write("config.yaml", format!("lr: {i}\n"));
        r.write("src/model.py", format!("x = {i}\n"));
        chain.push(r.run(&["-m", &format!("step {i}")], "true"));
    }
    r.write("REPORT.md", "# report CURRENT\n");
    r.ok(&["export", "--path", &format!("{}..{}", chain[0], chain[3]), "--branch", "chain"]);
    let revs: Vec<String> = git(&r, "rev-list --reverse chain ^main").lines().map(String::from).collect();
    let revs = if revs.len() == 4 { revs } else { git(&r, "rev-list --reverse chain").lines().map(String::from).collect() };
    assert_eq!(revs.len(), 4, "expected 4 commits on the exported branch, got {}", revs.len());
    assert_eq!(git(&r, "rev-list --min-parents=2 --count chain"), "0", "export history is not linear");
    for (i, (rev, id)) in revs.iter().zip(&chain).enumerate() {
        let subject = git(&r, &format!("log -1 --format=%s {rev}"));
        assert!(subject.contains(id.as_str()), "commit {i} subject lacks node id {id}: {subject}");
        let files = git(&r, &format!("ls-tree -r --name-only {rev}"));
        assert!(files.contains(".pollard-recipe.json"), "commit {i} lacks .pollard-recipe.json:\n{files}");
        assert!(files.contains("REPORT.md"), "commit {i} lacks the off-tree REPORT.md:\n{files}");
        assert_eq!(git(&r, &format!("show {rev}:REPORT.md")), "# report CURRENT", "v3 §7: off-tree files come from the current working copy");
        let recipe = git(&r, &format!("show {rev}:.pollard-recipe.json"));
        assert!(recipe.trim_start().starts_with('{'), "recipe file not JSON:\n{recipe}");
        if i > 0 {
            assert_eq!(git(&r, &format!("show {rev}:src/model.py")), format!("x = {i}"), "commit {i} has wrong code");
        }
    }
    assert_eq!(git(&r, "rev-parse HEAD"), head, "HEAD moved");
}

#[test]
fn export_path_accepts_pin_names() {
    let r = git_repo();
    let root = r.ok(&["init", "--from-git"]).last_line();
    r.write("config.yaml", "lr: 1\n");
    let a = r.run(&["-m", "a"], "true");
    r.ok(&["pin", &a, "paper-v1"]);
    r.ok(&["export", "--path", &format!("{root}..paper-v1"), "--branch", "paper-v1"]);
    let n = [git(&r, "rev-list --count paper-v1 ^main"), git(&r, "rev-list --count paper-v1")];
    assert!(n.iter().any(|c| c == "2"), "expected a 2-commit export for root..paper-v1, got {n:?}");
}

#[test]
fn pollard_never_touches_git_index_outside_export() {
    let r = git_repo();
    r.ok(&["init", "--from-git"]);
    let idx = std::fs::read(r.root.join(".git/index")).unwrap();
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "a"], "true");
    r.ok(&["tree"]);
    assert!(std::fs::read(r.root.join(".git/index")).unwrap() == idx, "git index changed by run/tree");
}
