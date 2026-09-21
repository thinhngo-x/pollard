//! Phase 1 (format 2), BACKLOG F1: format version, refusing newer formats.
mod common;
use common::*;

const NEWER_REPO: &str = "error: this repo uses format 3, written by a newer pollard; this pollard (0.2.0-alpha.1) reads up to format 2. Upgrade pollard.";

/// F1: `po init` creates a format-2 repo directly: state.sqlite at user_version 2, a
/// `db.sqlite/` stub directory, no backup, no migration notice.
#[test]
fn f1_init_creates_format_2_directly() {
    let r = Repo::bare();
    r.write("config.yaml", "lr: 1\n");
    let o = r.ok(&["init"]);
    assert!(r.state_db().is_file(), ".pollard/state.sqlite missing\n{o}");
    assert_eq!(r.user_version(), 2, "user_version of a fresh repo");
    assert!(
        r.root.join(".pollard/db.sqlite").is_dir(),
        ".pollard/db.sqlite should be the stub directory\n{o}"
    );
    assert!(
        !r.root.join(".pollard/backup").exists(),
        "fresh init made a backup\n{o}"
    );
    assert!(
        !o.all().contains("upgraded this repo"),
        "fresh init printed the migration notice\n{o}"
    );
    // and it works
    r.run(&["-m", "a"], "true");
    assert!(!r.po(&["tree"]).all().contains("upgraded this repo"));
}

/// F1: a repo at user_version 3 is refused by every command, exact message, nothing written.
#[test]
fn f1_newer_repo_format_refused_and_untouched() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.sql("PRAGMA user_version = 3;");
    let before = r.dot_state();
    for args in [
        vec!["tree"],
        vec!["tree", "--all"],
        vec!["show", a.as_str()],
        vec!["siblings"],
        vec!["log", a.as_str()],
        vec!["op", "log"],
        vec!["run", "--", "true"],
        vec!["pin", a.as_str(), "x"],
        vec!["prune", a.as_str()],
        vec!["note", a.as_str(), "x"],
        vec!["undo"],
        vec!["gc"],
        vec!["push"],
        vec!["pull"],
    ] {
        let o = r.fail(&args);
        assert_eq!(o.stderr.trim_end(), NEWER_REPO, "stderr of {args:?}\n{o}");
        let d = state_diff(&before, &r.dot_state());
        assert!(d.is_empty(), "{args:?} changed .pollard/:\n{d}\n{o}");
    }
}

/// F1: `--version` and `--help` never open or migrate a repo (run on the unmigrated fixture).
#[test]
fn f1_version_and_help_do_not_open_or_migrate() {
    let f = Fixture::new();
    let r = &f.repo;
    let before = r.dot_state();
    for args in [
        vec!["--version"],
        vec!["-V"],
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["tree", "--help"],
        vec!["prune", "--help"],
    ] {
        let o = r.ok(&args);
        assert!(
            !o.all().contains("upgraded"),
            "{args:?} printed the migration notice\n{o}"
        );
        let d = state_diff(&before, &r.dot_state());
        assert!(d.is_empty(), "{args:?} touched .pollard/:\n{d}\n{o}");
    }
    assert!(r.root.join(".pollard/db.sqlite").is_file());
}
