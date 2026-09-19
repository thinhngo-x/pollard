//! Phase 1 (format 2): the published alpha.1 binary fails loudly on format-2 data
//! (BACKLOG F4, F11). CI-only: every test needs `POLLARD_ALPHA1_BIN` and skips without it.
mod common;
use common::*;

/// F4: on a migrated copy of F0, each alpha.1 command exits non-zero and `.pollard/` stays
/// byte-identical; the error mentions the database.
#[test]
fn f4_alpha1_fails_on_migrated_repo_and_writes_nothing() {
    let Some(a1) = alpha1_bin("f4_alpha1_fails_on_migrated_repo_and_writes_nothing") else {
        return;
    };
    let f = Fixture::new();
    let r = &f.repo;
    r.ok(&["tree"]);
    let best = f.id("best");
    let base = f.id("base");
    for args in [
        vec!["tree"],
        vec!["show", best.as_str()],
        vec!["run", "--", "true"],
        vec!["pin", base.as_str(), "x"],
        vec!["prune", base.as_str()],
        vec!["undo"],
        vec!["push"],
    ] {
        let before = r.dot_state();
        let remote_before = file_state(&f.remote);
        let o = r.po_with(&a1, &args);
        assert!(
            !o.ok(),
            "alpha.1 {args:?} succeeded on a migrated repo:\n{o}"
        );
        let d = bytes_diff(&before, &r.dot_state());
        assert!(
            d.is_empty(),
            "alpha.1 {args:?} changed .pollard/:\n{d}\n{o}"
        );
        assert!(
            bytes_diff(&remote_before, &file_state(&f.remote)).is_empty(),
            "alpha.1 {args:?} changed the remote"
        );
        assert!(
            o.stderr.contains("database"),
            "alpha.1 {args:?} error should mention the database:\n{o}"
        );
    }
    // Still a working format-2 repo afterwards.
    assert!(r.ok(&["tree"]).stdout.contains(&f.id("late")));
}

/// F11: against a remote converted by F9, alpha.1 push and pull exit non-zero, and both the
/// remote and the alpha.1 clone's `.pollard/` are byte-identical afterwards.
#[test]
fn f11_alpha1_fails_on_converted_remote() {
    let Some(a1) = alpha1_bin("f11_alpha1_fails_on_converted_remote") else {
        return;
    };
    let f = Fixture::new();
    let old = f.copy_repo("old"); // still alpha.1 format, same remote (../remote)
    f.repo.ok(&["tree"]);
    f.repo.ok(&["push"]);
    assert!(f.remote.join("FORMAT").is_file(), "remote not converted");
    // Something new on the alpha.1 side, so its push has lines to send.
    old.write("config.yaml", "lr: 99\n");
    let o = old.po_with(&a1, &["run", "-m", "old client", "--", "sh", "train.sh"]);
    assert!(o.ok(), "alpha.1 run in its own repo:\n{o}");
    for cmd in ["push", "pull"] {
        let local = old.dot_state();
        let remote = file_state(&f.remote);
        let o = old.po_with(&a1, &[cmd]);
        assert!(
            !o.ok(),
            "alpha.1 {cmd} succeeded on a converted remote:\n{o}"
        );
        let d = bytes_diff(&remote, &file_state(&f.remote));
        assert!(d.is_empty(), "alpha.1 {cmd} changed the remote:\n{d}\n{o}");
        let d = bytes_diff(&local, &old.dot_state());
        assert!(
            d.is_empty(),
            "alpha.1 {cmd} changed its .pollard/:\n{d}\n{o}"
        );
    }
}
