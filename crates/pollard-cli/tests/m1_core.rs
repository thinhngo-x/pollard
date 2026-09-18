//! M1: node model, SQLite store, op log, `init`, `run` (code + config + notes), `tree`, `show`,
//! `note`, `undo`. SPEC §9 M1 + the cross-cutting rules that apply to these commands.
mod common;
use common::*;
use std::time::Duration;

#[test]
fn init_creates_repo_layout_and_gitignore_entry() {
    let r = Repo::bare();
    r.write(".gitignore", "target/\n");
    r.ok(&["init"]);
    assert!(r.root.join(".pollard").is_dir(), ".pollard/ missing");
    assert!(
        r.root.join(".pollard/db.sqlite").is_file(),
        ".pollard/db.sqlite missing (§3)"
    );
    let gi = r.read(".gitignore");
    assert!(
        gi.contains("target/"),
        "init clobbered existing .gitignore:\n{gi}"
    );
    assert!(
        gi.lines().any(|l| l.trim().trim_matches('/') == ".pollard"),
        ".pollard not added to .gitignore:\n{gi}"
    );
}

#[test]
fn init_twice_fails() {
    let r = Repo::init();
    r.fail(&["init"]);
}

#[test]
fn commands_outside_repo_fail_with_message() {
    let r = Repo::bare();
    let o = r.fail(&["tree"]);
    assert!(!o.stderr.trim().is_empty(), "no error message:\n{o}");
}

/// §9 M1: "3 runs on a 200-file tree take < 1 s each".
#[test]
fn three_runs_on_200_file_tree_under_1s_each() {
    let r = Repo::bare();
    write_200_files(&r);
    r.write("config.yaml", "lr: 3\n");
    r.ok(&["init"]);
    for i in 0..3 {
        r.write("src/pkg0/mod0.py", format!("# edit {i}\n"));
        let o = r.run_out(&["-m", &format!("run {i}")], "true");
        assert!(o.ok() && is_node_id(&o.last_line()), "{o}");
        // Timing budget: spec < 1 s (release); see common::budget.
        assert!(
            o.elapsed < budget(Duration::from_secs(1)),
            "run {i} took {:?} (spec: < 1 s)\n{o}",
            o.elapsed
        );
    }
}

#[test]
fn run_last_line_is_node_id_of_word_word_counter_shape() {
    let r = Repo::init();
    let id = r.run(&["-m", "first"], "echo from-script");
    assert!(is_node_id(&id));
    let id2 = r.run(&["-m", "second", "--force"], "true");
    assert_ne!(id, id2, "ids must be unique");
}

/// §9 M1: "`tree` shows parent links and note titles".
#[test]
fn tree_shows_parent_links_and_note_titles() {
    let r = Repo::init();
    let a = r.run(&["-m", "alpha title", "-m", "alpha body paragraph"], "true");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    let b = r.run(&["-m", "beta title"], "true");
    r.write("config.yaml", "lr: 2\ndepth: 12\nact: relu\n");
    let c = r.run(&["-m", "gamma title", "--parent", &a], "true");

    let t = r.ok(&["tree"]).stdout;
    for (id, title) in [(&a, "alpha title"), (&b, "beta title"), (&c, "gamma title")] {
        let line = line_with(&t, id).unwrap_or_else(|| panic!("{id} missing from tree:\n{t}"));
        assert!(
            line.contains(title),
            "note title {title:?} not on {id}'s tree line:\n{t}"
        );
    }
    assert!(
        !t.contains("alpha body paragraph"),
        "tree must show only the first line of the note:\n{t}"
    );
    // Parent links: children are indented deeper than their parent, and siblings b/c share a depth.
    assert!(col_of(&t, &b) > col_of(&t, &a), "b not drawn under a:\n{t}");
    assert!(col_of(&t, &c) > col_of(&t, &a), "c not drawn under a:\n{t}");
    assert_eq!(
        col_of(&t, &b),
        col_of(&t, &c),
        "siblings b,c at different depths:\n{t}"
    );
    // show carries the parent explicitly.
    let sb = r.ok(&["show", &b]).stdout;
    assert!(
        sb.contains(&a),
        "show {b} does not mention parent {a}:\n{sb}"
    );
}

/// §9 M1: "an `-m`-less run gets an `auto` note" (auto-generated from the config delta, §3).
#[test]
fn run_without_m_gets_auto_note_from_config_delta() {
    let r = Repo::init();
    r.run(&["-m", "base"], "true");
    r.write("config.yaml", "lr: 1\ndepth: 12\nact: relu\n");
    let id = r.run(&[], "true");
    let s = r.ok(&["show", &id]).stdout;
    assert!(
        s.to_lowercase().contains("auto"),
        "auto note not flagged `auto` in show:\n{s}"
    );
    assert!(
        s.contains("lr"),
        "auto note does not mention the changed key `lr`:\n{s}"
    );
    let t = r.ok(&["tree"]).stdout;
    let line = line_with(&t, &id).unwrap();
    assert!(line.contains("lr"), "auto note title not in tree:\n{t}");
}

#[test]
fn m_dash_reads_note_from_stdin() {
    let r = Repo::init();
    let mut c = r.cmd_in(&r.root, &bin(), &["run", "-m", "-", "--", "true"]);
    c.stdin(std::process::Stdio::piped());
    let mut child = c
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin title\nbody\n")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = String::from_utf8_lossy(&o.stdout)
        .lines()
        .last()
        .unwrap()
        .trim()
        .to_string();
    let t = r.ok(&["tree"]).stdout;
    assert!(
        line_with(&t, &id).unwrap().contains("from stdin title"),
        "{t}"
    );
}

/// §9 M1: "`undo` after `run` removes the node and restores the working copy".
#[test]
fn undo_after_run_removes_node_and_restores_working_copy() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 9\ndepth: 12\nact: relu\n");
    r.write("new_file.py", "print(1)\n");
    let before = r.snapshot();
    let b = r.run(&["-m", "b"], "true");
    let o = r.ok(&["undo"]);
    r.fail(&["show", &b]);
    let t = r.ok(&["tree"]).stdout;
    assert!(!t.contains(&b), "undone node still in tree:\n{t}");
    assert_eq!(
        r.current(),
        a,
        "current node not restored to parent after undo"
    );
    let after = r.snapshot();
    let names = |v: &Vec<(String, Vec<u8>)>| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&after),
        names(&before),
        "working-copy file set differs after undo\n{o}"
    );
    assert!(after == before, "working-copy contents differ after undo");
}

#[test]
fn undo_n_reverses_last_n_ops() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "b"], "true");
    r.write("config.yaml", "lr: 2\n");
    let c = r.run(&["-m", "c"], "true");
    r.ok(&["undo", "2"]);
    r.ok(&["show", &a]);
    r.fail(&["show", &b]);
    r.fail(&["show", &c]);
}

#[test]
fn op_log_lists_mutating_commands() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.ok(&["note", &a, "renamed"]);
    let log = r.ok(&["op", "log"]).stdout;
    assert!(log.contains("run"), "op log lacks the run:\n{log}");
    assert!(log.contains("note"), "op log lacks the note:\n{log}");
}

/// §9 M1 (v3): "duplicate recipe of a `done` node is refused without `--force`".
#[test]
fn duplicate_recipe_refused_without_force() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    let o = r.run_out(&["-m", "again"], "true");
    assert!(!o.ok(), "duplicate recipe accepted:\n{o}");
    assert!(
        o.all().contains(&a),
        "refusal must name the existing node {a} (§8 errors name the node):\n{o}"
    );
    let t = r.ok(&["tree"]).stdout;
    assert_eq!(
        ids_in(&t).len(),
        1,
        "refused run still created a node:\n{t}"
    );
    let b = r.run(&["-m", "again", "--force"], "true");
    assert_ne!(a, b);
}

#[test]
fn duplicate_check_ignores_parent_and_note_but_not_code() {
    let r = Repo::init();
    r.run(&["-m", "a"], "true");
    r.write("model.py", "x = 2\n");
    r.run(&["-m", "code changed"], "true");
    // back to the original recipe from a different parent → still a duplicate of the first node.
    r.write("model.py", "x = 1\n");
    let o = r.run_out(&["-m", "revert"], "true");
    assert!(!o.ok(), "recipe identical to node a must be refused:\n{o}");
}

#[test]
fn new_untracked_file_is_part_of_the_recipe() {
    let r = Repo::init();
    r.run(&["-m", "a"], "true");
    r.write("brand_new.py", "pass\n");
    r.run(&["-m", "with new file"], "true"); // not a duplicate
}

#[test]
fn run_status_done_and_failed() {
    let r = Repo::init();
    let a = r.run(&["-m", "ok"], "exit 0");
    assert!(r.ok(&["show", &a]).stdout.contains("done"));
    r.write("model.py", "x = 3\n");
    let o = r.run_out(&["-m", "boom"], "exit 3");
    let id = o.last_line();
    assert!(
        is_node_id(&id),
        "failed run must still print its node id last:\n{o}"
    );
    let s = r.ok(&["show", &id]).stdout;
    assert!(s.contains("failed"), "exit 3 not recorded as failed:\n{s}");
}

#[test]
fn show_prints_full_record() {
    let r = Repo::init();
    let a = r.run(&["-m", "title line", "-m", "body line"], "true");
    let s = r.ok(&["show", &a]).stdout;
    for needle in [a.as_str(), "title line", "body line", "done"] {
        assert!(s.contains(needle), "show lacks {needle:?}:\n{s}");
    }
    // recipe hashes: a 64-hex blake3 (config/data/env) and a 40-hex git tree hash (code).
    let hexes: Vec<usize> = s
        .split(|c: char| !c.is_ascii_hexdigit())
        .map(str::len)
        .collect();
    assert!(
        hexes.iter().any(|&n| n == 40),
        "no 40-hex git tree hash in show (§7 code hash):\n{s}"
    );
    assert!(
        hexes.iter().any(|&n| n == 64 || (8..40).contains(&n)),
        "no blake3 hash (full or abbreviated) in show:\n{s}"
    );
}

#[test]
fn note_sets_and_replaces_note_last_line_is_id_and_undo_restores() {
    let r = Repo::init();
    let a = r.run(&["-m", "original"], "true");
    let o = r.ok(&["note", &a, "replaced title"]);
    assert_eq!(o.last_line(), a, "note must print the node id last:\n{o}");
    let t = r.ok(&["tree"]).stdout;
    assert!(line_with(&t, &a).unwrap().contains("replaced title"), "{t}");
    r.ok(&["undo"]);
    let t = r.ok(&["tree"]).stdout;
    assert!(
        line_with(&t, &a).unwrap().contains("original"),
        "undo did not restore note:\n{t}"
    );
}

#[test]
fn note_e_uses_editor() {
    let r = Repo::init();
    let a = r.run(&["-m", "original"], "true");
    let ed = r.script("ed.sh", "echo 'edited via editor' > \"$1\"");
    let mut c = r.cmd_in(&r.root, &bin(), &["note", &a, "-e"]);
    c.env("EDITOR", &ed).env("VISUAL", &ed);
    let o = r.exec(c);
    assert!(o.ok(), "{o}");
    assert_eq!(o.last_line(), a);
    assert!(r.ok(&["show", &a]).stdout.contains("edited via editor"));
}

/// §4: node refs accept full id, unique prefix, `@`, `@-` (pin names are covered in m8).
#[test]
fn node_references_full_prefix_at_and_at_minus() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "b"], "true");
    assert_eq!(r.current(), b, "`@` is not the node just run");
    assert_eq!(
        first_id(&r.ok(&["show", "@-"]).stdout).unwrap(),
        a,
        "`@-` is not the parent"
    );
    // shortest prefix of b that a does not share
    let p = (1..=b.len())
        .map(|n| &b[..n])
        .find(|p| !a.starts_with(p))
        .unwrap();
    assert_eq!(
        first_id(&r.ok(&["show", p]).stdout).unwrap(),
        b,
        "unique prefix {p:?} did not resolve"
    );
    r.fail(&["show", "no-such-node-99"]);
}

#[test]
fn run_parent_flag_sets_parent() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    let _b = r.run(&["-m", "b"], "true");
    r.write("config.yaml", "lr: 2\n");
    let c = r.run(&["-m", "c", "--parent", &a], "true");
    assert!(r.ok(&["show", &c]).stdout.contains(&a));
    let s = r.ok(&["show", "@-"]).stdout;
    assert_eq!(first_id(&s).unwrap(), a, "@- after --parent run is not {a}");
}

#[test]
fn run_records_exact_command() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    let s = r.ok(&["show", &a]).stdout;
    assert!(
        s.contains("sh ") && s.contains("run"),
        "show does not include the launch command:\n{s}"
    );
}

#[test]
fn po_alias_is_built() {
    let po = assert_cmd::cargo::cargo_bin("po");
    assert!(po.exists(), "`po` alias binary not built");
    let r = Repo::bare();
    let o = r.exec(r.cmd_in(&r.root, &po, &["init"]));
    assert!(o.ok(), "{o}");
    assert!(r.root.join(".pollard").is_dir());
}

/// v3 §4: a match that is `failed`/`killed`/`pruned` only prints a notice naming it.
#[test]
fn duplicate_of_failed_node_is_allowed_with_notice() {
    let r = Repo::init();
    let o = r.run_out(&["-m", "crashed"], "exit 1");
    let failed = o.last_line();
    assert!(is_node_id(&failed), "{o}");
    let o = r.run_out(&["-m", "relaunch"], "true");
    assert!(
        o.ok(),
        "relaunching a failed recipe must not be refused:\n{o}"
    );
    assert!(
        o.all().contains(&failed),
        "no notice naming the failed duplicate {failed}:\n{o}"
    );
    assert_ne!(o.last_line(), failed);
}

// ---------- plain fork (M1 in v3) ----------

#[test]
fn fork_restores_code_and_config_and_prints_id() {
    let r = Repo::init();
    r.write("util.py", "def u(): pass\n");
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    r.write("attn.py", "class Attn: pass\n");
    r.rm("util.py");
    let b = r.run(&["-m", "b"], "true");
    let o = r.ok(&["fork", &a]);
    assert_eq!(o.last_line(), a, "fork must print the node id last:\n{o}");
    assert_eq!(
        r.read("config.yaml"),
        "lr: 3\ndepth: 12\nact: relu\n",
        "config not restored"
    );
    assert!(r.root.join("util.py").exists(), "util.py not restored");
    assert!(
        !r.root.join("attn.py").exists(),
        "attn.py (not in {a}) left behind"
    );
    assert_eq!(r.current(), a);
    r.ok(&["fork", &b]);
    assert_eq!(r.read("config.yaml"), "lr: 1\n");
    assert!(r.root.join("attn.py").exists() && !r.root.join("util.py").exists());
}

/// §9 M1 (v3): "`fork` then `undo` restores uncommitted working-copy edits byte-identically".
#[test]
fn fork_then_undo_restores_uncommitted_edits_byte_identically() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("model.py", "x = 2\n");
    let b = r.run(&["-m", "b"], "true");
    r.write("model.py", "x = UNSAVED WORK\r\n\ttrailing \n");
    r.write("wip.py", "draft\n");
    r.write("blob.bin", prng_bytes(300_000, 7));
    r.rm("config.yaml");
    let before = r.snapshot();
    r.ok(&["fork", &a]);
    assert_eq!(r.read("model.py"), "x = 1\n");
    r.ok(&["undo"]);
    let after = r.snapshot();
    let names = |v: &Vec<(String, Vec<u8>)>| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&after),
        names(&before),
        "file set differs after fork+undo"
    );
    assert!(
        after == before,
        "uncommitted edits not restored byte-identically after fork+undo"
    );
    assert_eq!(
        r.current(),
        b,
        "undo of fork did not restore the current node"
    );
}

/// §9 M1 (v3): "a node's `code` equals `git write-tree` for the same files".
#[test]
fn code_hash_equals_git_write_tree() {
    let r = Repo::init();
    r.write("src/pkg/a.py", "a = 1\n");
    r.write("src/pkg/b.py", "b = 2\n");
    r.write("run.sh", "#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            r.root.join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let a = r.run(&["-m", "a"], "true");
    let tree = r
        .sh("git init -q . && git add -A && git write-tree")
        .stdout
        .trim()
        .to_string();
    let s = r.ok(&["show", &a]).stdout;
    assert!(
        s.contains(&tree),
        "code hash in `show {a}` != `git write-tree` {tree}:\n{s}"
    );
}
