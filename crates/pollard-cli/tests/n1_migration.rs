//! Phase 1 (format 2): migrating the alpha.1 fixture (BACKLOG F2, F3, F5, F6, F7, F8).
//! The fixture is `tests/fixtures/alpha1/` (F0); roles are in golden/ids.json.
mod common;
use common::*;
use std::process::Stdio;

const NOTICE_HEAD: &str = "pollard: upgraded this repo to format 2 (pollard 0.2.0-alpha.1)";
const NOTICE_UNDO: &str = "  po undo cannot go back past this upgrade, and pollard 0.1.0-alpha.1 can no longer open this repo";
const NOTICE_ROLLBACK: &str = "  to roll back, see the upgrade notes: https://github.com/thinhngo-x/pollard/blob/main/CHANGELOG.md#upgrade-notes";
const UPGRADED_TXT: &str = "This repo was upgraded to format 2 by pollard 0.2.0-alpha.1. The database is now .pollard/state.sqlite.
Install pollard 0.2.0-alpha.1 or newer to use it.
The old database is in .pollard/backup/. To roll back, see the upgrade notes in pollard's CHANGELOG.md.";

/// F3 crash-safety hook steps (`POLLARD_TEST_MIGRATION_ABORT=<step>`, debug builds only).
/// BACKLOG names the steps but not their spelling; these are the names the tests use.
const ABORT_STEPS: [&str; 4] = ["tmp_written", "tmp_migrated", "renamed", "old_moved"];

/// Pruned roles in the fixture: (role, expected status after migration, prune command whose
/// op timestamp becomes `pruned_at`).
fn recovered(f: &Fixture) -> Vec<(&'static str, &'static str, String)> {
    let mid = format!("pollard prune {}", f.id("mid"));
    vec![
        ("mid", "done", mid.clone()),
        ("best", "done", mid.clone()),
        ("crashed", "failed", mid.clone()),
        ("stopped", "killed", mid),
        (
            "kept",
            "done",
            format!("pollard prune {} --keep-weights", f.id("kept")),
        ),
        // newest `prune redo` op (prune → undo → prune)
        ("redo", "done", format!("pollard prune {}", f.id("redo"))),
    ]
}

fn golden_field(file: &str, key: &str) -> String {
    golden(file)
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some(key)).then(|| it.collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_else(|| panic!("no `{key}` line in golden/{file}"))
}

/// The migration notice lines in `stderr` (from the head line through the rollback line).
fn notice_lines(stderr: &str) -> Vec<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let Some(start) = lines.iter().position(|l| *l == NOTICE_HEAD) else {
        return vec![];
    };
    let end = lines[start..]
        .iter()
        .position(|l| l.starts_with("  to roll back"))
        .map(|e| start + e)
        .unwrap_or(lines.len() - 1);
    lines[start..=end].iter().map(|s| s.to_string()).collect()
}

/// Assert the full notice for the F0 fixture (BACKLOG "Messages").
fn assert_fixture_notice(f: &Fixture, o: &Out, backup: &str) {
    let n = notice_lines(&o.stderr);
    assert_eq!(n.len(), 6, "migration notice should have 6 lines:\n{o}");
    assert_eq!(n[0], NOTICE_HEAD, "{o}");
    assert_eq!(
        n[1],
        format!("  backup of the old database: {backup}"),
        "{o}"
    );
    // Optional lines, indented two spaces (Lead dev). N counts the guessed node too: 7.
    assert_eq!(
        n[2], "  pruned is now a flag: 7 pruned nodes keep their done/failed/killed status",
        "{o}"
    );
    assert_eq!(
        n[3],
        format!(
            "  guessed status (no op-log record): {} (done)",
            f.id("ghost")
        ),
        "{o}"
    );
    assert_eq!(n[4], NOTICE_UNDO, "{o}");
    assert_eq!(n[5], NOTICE_ROLLBACK, "{o}");
}

fn notice_count(o: &Out) -> usize {
    o.stderr.matches("upgraded this repo to format 2").count()
}

fn migrated() -> (Fixture, Out) {
    let f = Fixture::new();
    let o = f.repo.ok(&["tree"]);
    (f, o)
}

// ------------------------------------------------------------------------------------------
// F2. Automatic migration
// ------------------------------------------------------------------------------------------

/// F2: `po tree` migrates once, notice on stderr before its own output, exit 0, user_version 2.
#[test]
fn f2_tree_migrates_with_notice_first() {
    let f = Fixture::new();
    let r = &f.repo;
    let merged = r.exec(r.cmd_in(
        &r.root,
        std::path::Path::new("sh"),
        &["-c", "\"$POLLARD_BIN\" tree 2>&1"],
    ));
    assert!(merged.ok(), "{merged}");
    let text = &merged.stdout;
    let notice_at = text
        .find(NOTICE_HEAD)
        .unwrap_or_else(|| panic!("no migration notice:\n{merged}"));
    let tree_at = text
        .find(&f.id("root"))
        .unwrap_or_else(|| panic!("no tree output:\n{merged}"));
    assert!(
        notice_at < tree_at,
        "notice must come before tree output:\n{text}"
    );
    assert_eq!(r.user_version(), 2);
}

#[test]
fn f2_notice_text_exact() {
    let (f, o) = migrated();
    assert_fixture_notice(&f, &o, ".pollard/backup/db-format1.sqlite");
    assert!(
        !o.stdout.contains("upgraded"),
        "notice must be on stderr only\n{o}"
    );
}

/// F2: any command triggers the migration, including read-only ones.
#[test]
fn f2_read_only_commands_also_migrate() {
    let probe = Fixture::new();
    let (root, base, best) = (probe.id("root"), probe.id("base"), probe.id("best"));
    for args in [
        vec!["show", best.as_str()],
        vec!["siblings", root.as_str()],
        vec!["log", base.as_str(), "--key", "loss"],
        vec!["op", "log"],
        vec!["tree", "--all"],
    ] {
        let f = Fixture::new();
        let o = f.repo.ok(&args);
        assert_eq!(notice_count(&o), 1, "{args:?} did not migrate:\n{o}");
        assert_eq!(f.repo.user_version(), 2, "{args:?}");
    }
}

/// F2: no `pruned` status is left; statuses come back from the prune op, `pruned_at` is that
/// op's timestamp (for redo, the second prune).
#[test]
fn f2_statuses_recovered_from_prune_ops() {
    let (f, _) = migrated();
    let r = &f.repo;
    assert_eq!(
        r.sql("SELECT count(*) FROM nodes WHERE status='pruned';")
            .trim(),
        "0",
        "status 'pruned' survived the migration"
    );
    for (role, status, cmd) in recovered(&f) {
        let (s, p) = r.status_of(&f.id(role));
        assert_eq!(s, status, "{role} status");
        assert_eq!(
            p,
            Fixture::op_ts(&cmd),
            "{role} pruned_at should be the ts of `{cmd}`"
        );
    }
}

/// F2 / D4: ghost (pulled already pruned, no prune op here) gets the fallback.
#[test]
fn f2_ghost_gets_fallback_status() {
    let (f, o) = migrated();
    let (s, p) = f.repo.status_of(&f.id("ghost"));
    assert_eq!(s, "done", "ghost has finished_at, so fallback is done");
    assert_eq!(
        p,
        golden_field("show_ghost.txt", "finished"),
        "ghost pruned_at = finished_at"
    );
    assert!(
        o.stderr.contains(&format!("{} (done)", f.id("ghost"))),
        "notice does not list ghost as guessed:\n{o}"
    );
}

#[test]
fn f2_late_never_pruned() {
    let (f, _) = migrated();
    assert_eq!(
        f.repo.status_of(&f.id("late")),
        ("done".to_string(), String::new())
    );
    for role in ["root", "base"] {
        assert_eq!(f.repo.status_of(&f.id(role)).1, "", "{role} pruned_at");
    }
}

/// F2: `po show best` prints `status done` and `pruned <ts>` as two facts.
#[test]
fn f2_show_prints_status_and_pruned_separately() {
    let (f, _) = migrated();
    let s = f.repo.ok(&["show", &f.id("best")]).stdout;
    let ts = Fixture::op_ts(&format!("pollard prune {}", f.id("mid")));
    let words: Vec<Vec<&str>> = s.lines().map(|l| l.split_whitespace().collect()).collect();
    assert!(
        words.contains(&vec!["status", "done"]),
        "no `status  done` line:\n{s}"
    );
    assert!(
        words.contains(&vec!["pruned", ts.as_str()]),
        "no `pruned  {ts}` line:\n{s}"
    );
    // A node that is not pruned has no pruned line.
    let s = f.repo.ok(&["show", &f.id("late")]).stdout;
    assert!(
        !s.lines().any(|l| l.starts_with("pruned")),
        "late is not pruned:\n{s}"
    );
}

/// F2: every id, parent, recipe hash, note, fork_step, metric and weights hash unchanged:
/// `po show <each>` minus the status/pruned lines equals alpha.1's.
#[test]
fn f2_show_matches_golden_except_status() {
    let (f, _) = migrated();
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("status") && !l.starts_with("pruned"))
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (role, id) in f.roles() {
        let now = f.repo.ok(&["show", &id]).stdout;
        let then = golden(&format!("show_{role}.txt"));
        assert_eq!(
            strip(&now),
            strip(&then),
            "`po show {role}` differs from alpha.1 (besides status/pruned)"
        );
    }
}

/// F2: `siblings --json` byte-identical to alpha.1 (public format, frozen since M2).
#[test]
fn f2_siblings_json_byte_identical() {
    let (f, _) = migrated();
    for role in ["base", "root"] {
        let o = f.repo.ok(&["siblings", &f.id(role), "--json"]);
        assert_eq!(
            o.stdout,
            golden(&format!("siblings_{role}.json")),
            "siblings {role} --json changed"
        );
    }
}

#[test]
fn f2_pins_resolve_to_same_nodes() {
    let (f, _) = migrated();
    for (pin, role) in [("paper", "best"), ("best-v1", "best"), ("root-pin", "root")] {
        let s = f.repo.ok(&["show", pin]).stdout;
        assert_eq!(
            first_id(&s),
            Some(f.id(role)),
            "pin {pin} should show {role}:\n{s}"
        );
    }
}

/// F2: a second command prints no notice and changes nothing.
#[test]
fn f2_second_command_is_noop() {
    let (f, _) = migrated();
    let r = &f.repo;
    let before = r.dot_state();
    for args in [vec!["tree"], vec!["tree", "--all"], vec!["op", "log"]] {
        let o = r.ok(&args);
        assert_eq!(notice_count(&o), 0, "second command re-migrated:\n{o}");
    }
    let o = r.ok(&["show", &f.id("best")]);
    assert_eq!(notice_count(&o), 0, "{o}");
    let d = content_diff(&before, &r.dot_state());
    assert!(
        d.is_empty(),
        "read-only commands after migration changed .pollard/:\n{d}"
    );
    assert_eq!(r.user_version(), 2);
}

/// F2: two `po tree` started at the same moment on an unmigrated copy: both exit 0, one
/// notice in total, one backup file.
#[test]
fn f2_concurrent_first_open() {
    for round in 0..5 {
        let f = Fixture::new();
        let r = &f.repo;
        let spawn = || {
            let mut c = r.cmd_in(&r.root, &bin(), &["tree"]);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c.spawn().unwrap()
        };
        let (a, b) = (spawn(), spawn());
        let outs: Vec<_> = [a, b]
            .into_iter()
            .map(|c| c.wait_with_output().unwrap())
            .collect();
        let mut notices = 0;
        for o in &outs {
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(
                o.status.success(),
                "round {round}: a concurrent `po tree` failed:\n{err}"
            );
            notices += err.matches("upgraded this repo to format 2").count();
        }
        assert_eq!(notices, 1, "round {round}: expected exactly one notice");
        let backups = count_files(&r.root.join(".pollard/backup"));
        assert_eq!(backups, 1, "round {round}: expected one backup file");
        assert_eq!(r.user_version(), 2);
    }
}

// ------------------------------------------------------------------------------------------
// F3. Backup, crash safety, rollback
// ------------------------------------------------------------------------------------------

/// F3: the backup is a standalone format-1 database (no -wal/-shm needed).
#[test]
fn f3_backup_is_standalone_format1_copy() {
    let (f, _) = migrated();
    let backup = f.repo.root.join(".pollard/backup/db-format1.sqlite");
    assert!(backup.is_file(), "no backup at {}", backup.display());
    let alone = tempfile::tempdir().unwrap();
    let copy = alone.path().join("db.sqlite");
    std::fs::copy(&backup, &copy).unwrap();
    assert_eq!(sql(&copy, "PRAGMA user_version;").trim(), "0");
    let counts: String = ["nodes", "pins", "ops"]
        .iter()
        .map(|t| {
            format!(
                "{t} {}",
                sql(&copy, &format!("SELECT count(*) FROM {t};")).trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(counts, golden("counts.txt").trim_end(), "backup row counts");
    assert_eq!(
        sql(&copy, "SELECT id, status FROM nodes ORDER BY id;"),
        golden("status.txt"),
        "backup is not the alpha.1 database"
    );
}

/// F3 (CI-only part): alpha.1 opens the backup copied alone into a fresh alpha.1 layout.
#[test]
fn f3_backup_opens_in_alpha1() {
    let Some(a1) = alpha1_bin("f3_backup_opens_in_alpha1") else {
        return;
    };
    let (f, _) = migrated();
    let fresh = Fixture::new();
    let dot = fresh.repo.root.join(".pollard");
    std::fs::remove_file(dot.join("db.sqlite")).unwrap();
    std::fs::copy(
        f.repo.root.join(".pollard/backup/db-format1.sqlite"),
        dot.join("db.sqlite"),
    )
    .unwrap();
    let o = fresh.repo.po_with(&a1, &["tree", "--all"]);
    assert!(o.ok(), "{o}");
    assert_eq!(
        o.stdout,
        golden("tree_all.txt"),
        "alpha.1 tree --all of the backup"
    );
}

/// F3: layout after migration.
#[test]
fn f3_layout_after_migration() {
    let (f, o) = migrated();
    let dot = f.repo.root.join(".pollard");
    assert!(dot.join("state.sqlite").is_file(), "{o}");
    assert!(
        dot.join("db.sqlite").is_dir(),
        "db.sqlite should be a stub dir\n{o}"
    );
    let txt = std::fs::read_to_string(dot.join("db.sqlite/UPGRADED.txt"))
        .expect("db.sqlite/UPGRADED.txt missing");
    assert_eq!(txt.trim_end(), UPGRADED_TXT, "UPGRADED.txt text");
    for gone in ["db.sqlite-wal", "db.sqlite-shm", "state.sqlite.tmp"] {
        assert!(!dot.join(gone).exists(), ".pollard/{gone} left behind");
    }
}

/// F3: an existing backup is never overwritten; the new one is db-format1-<unix secs>.sqlite
/// and the notice names it.
#[test]
fn f3_existing_backup_not_overwritten() {
    let f = Fixture::new();
    let r = &f.repo;
    r.write(".pollard/backup/db-format1.sqlite", "sentinel");
    let o = r.ok(&["tree"]);
    assert_eq!(
        r.read(".pollard/backup/db-format1.sqlite"),
        "sentinel",
        "backup overwritten"
    );
    let names: Vec<String> = std::fs::read_dir(r.root.join(".pollard/backup"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "db-format1.sqlite")
        .collect();
    assert_eq!(names.len(), 1, "expected one new backup: {names:?}");
    let secs = names[0]
        .strip_prefix("db-format1-")
        .and_then(|s| s.strip_suffix(".sqlite"))
        .unwrap_or_else(|| panic!("bad backup name {}", names[0]));
    assert!(
        !secs.is_empty() && secs.chars().all(|c| c.is_ascii_digit()),
        "bad backup name {}",
        names[0]
    );
    assert_fixture_notice(&f, &o, &format!(".pollard/backup/{}", names[0]));
}

/// Everything a clean migration leaves, for comparing crash-recovered repos.
fn end_state(f: &Fixture) -> (String, String, String) {
    let r = &f.repo;
    (
        r.ok(&["tree", "--all"]).stdout,
        r.sql("SELECT id, status, coalesce(pruned_at,''), parent FROM nodes ORDER BY id;"),
        r.sql("SELECT coalesce(name,''), node_id FROM pins ORDER BY name;"),
    )
}

/// F3: crash safety. Abort at each step; the next plain `po tree` finishes, and the end state
/// equals a clean run (same tree, one backup, nothing lost).
#[cfg(debug_assertions)]
#[test]
fn f3_crash_at_each_step_recovers() {
    let clean = Fixture::new();
    clean.repo.ok(&["tree"]);
    let want = end_state(&clean);
    let want_ops = clean.repo.sql("SELECT count(*) FROM ops;");
    for step in ABORT_STEPS {
        let f = Fixture::new();
        let r = &f.repo;
        let o = r.po_env(&[("POLLARD_TEST_MIGRATION_ABORT", step)], &["tree"]);
        assert!(!o.ok(), "abort at {step} should exit non-zero:\n{o}");
        let o = r.po(&["tree"]);
        assert!(o.ok(), "tree after abort at {step}:\n{o}");
        assert_eq!(end_state(&f), want, "end state after abort at {step}");
        assert_eq!(
            r.sql("SELECT count(*) FROM ops;"),
            want_ops,
            "ops after {step}"
        );
        assert_eq!(
            count_files(&r.root.join(".pollard/backup")),
            1,
            "backups after abort at {step}"
        );
        assert_eq!(r.user_version(), 2);
        assert!(!r.root.join(".pollard/state.sqlite.tmp").exists(), "{step}");
        assert!(
            r.root.join(".pollard/db.sqlite/UPGRADED.txt").is_file(),
            "{step}"
        );
    }
}

/// F3 (CI-only): the CHANGELOG rollback steps verbatim, then alpha.1 reads the repo; running
/// 0.2 afterwards migrates again cleanly.
#[test]
fn f3_rollback_steps_restore_alpha1() {
    let Some(a1) = alpha1_bin("f3_rollback_steps_restore_alpha1") else {
        return;
    };
    let (f, _) = migrated();
    let r = &f.repo;
    let want = end_state(&f);
    r.sh("rm -r .pollard/db.sqlite .pollard/state.sqlite* && mv .pollard/backup/db-format1.sqlite .pollard/db.sqlite");
    let o = r.po_with(&a1, &["tree", "--all"]);
    assert!(o.ok(), "{o}");
    assert_eq!(
        o.stdout,
        golden("tree_all.txt"),
        "alpha.1 tree --all after rollback"
    );
    let o = r.ok(&["tree"]);
    assert_eq!(notice_count(&o), 1, "re-migration notice\n{o}");
    assert_eq!(end_state(&f), want, "re-migration end state");
}

// ------------------------------------------------------------------------------------------
// F5. Undo stops at the upgrade
// ------------------------------------------------------------------------------------------

#[test]
fn f5_undo_right_after_migration_refused() {
    let (f, _) = migrated();
    let r = &f.repo;
    let before = r.dot_state();
    let o = r.fail(&["undo"]);
    assert_eq!(
        o.stderr.trim_end(),
        "error: cannot undo past the format-2 upgrade (0 ops since it); nothing undone",
        "{o}"
    );
    let d = content_diff(&before, &r.dot_state());
    assert!(d.is_empty(), "refused undo changed .pollard/:\n{d}");
}

#[test]
fn f5_undo_across_barrier_refused_whole() {
    let (f, _) = migrated();
    let r = &f.repo;
    let base = f.id("base");
    let snap = || {
        (
            r.ok(&["tree", "--all"]).stdout,
            r.ok(&["show", &base]).stdout,
            r.sql("SELECT coalesce(name,''), node_id FROM pins ORDER BY name;"),
        )
    };
    let after_migration = snap();
    r.ok(&["note", &base, "edited after upgrade"]);
    r.ok(&["pin", &base, "extra"]);
    let o = r.fail(&["undo", "3"]);
    assert_eq!(
        o.stderr.trim_end(),
        "error: cannot undo past the format-2 upgrade (2 ops since it); nothing undone",
        "{o}"
    );
    assert!(
        r.ok(&["show", &base])
            .stdout
            .contains("edited after upgrade"),
        "undo 3 partly undid the note"
    );
    assert_eq!(
        first_id(&r.ok(&["show", "extra"]).stdout),
        Some(base.clone()),
        "undo 3 partly undid the pin"
    );
    r.ok(&["undo", "2"]);
    assert_eq!(
        snap(),
        after_migration,
        "undo 2 should restore the just-migrated state"
    );
}

#[test]
fn f5_op_log_lists_pre_migration_ops() {
    let (f, _) = migrated();
    let log = f.repo.ok(&["op", "log"]).stdout;
    for g in golden("op_log.txt").lines() {
        let w: Vec<&str> = g.split_whitespace().collect();
        let (ts, cmd) = (w[1], w[2..].join(" "));
        assert!(
            log.lines().any(|l| l.contains(ts) && l.contains(&cmd)),
            "pre-migration op `{ts} {cmd}` missing from op log:\n{log}"
        );
    }
}

// ------------------------------------------------------------------------------------------
// F6. Hidden work under a pruned node becomes visible
// ------------------------------------------------------------------------------------------

#[test]
fn f6_tree_shows_placeholder_with_late_under_it() {
    let (f, _) = migrated();
    let t = f.repo.ok(&["tree"]).stdout;
    let (mid, late) = (f.id("mid"), f.id("late"));
    let ml = line_with(&t, &mid).unwrap_or_else(|| panic!("mid placeholder missing:\n{t}"));
    assert!(
        ml.contains("done (pruned)"),
        "mid line should read `done (pruned)`:\n{t}"
    );
    let ll = line_with(&t, &late).unwrap_or_else(|| panic!("late missing from tree:\n{t}"));
    assert!(
        ll.contains("(@)") && ll.contains("done") && !ll.contains("(pruned)"),
        "{t}"
    );
    assert!(
        col_of(&t, &late) > col_of(&t, &mid),
        "late not under mid:\n{t}"
    );
    let pos = |id: &str| t.find(id).unwrap();
    assert!(pos(&late) > pos(&mid), "late listed before mid:\n{t}");
    for role in ["root", "base"] {
        assert!(t.contains(&f.id(role)), "{role} missing:\n{t}");
    }
    for role in ["best", "crashed", "stopped", "kept", "ghost", "redo"] {
        assert!(
            !t.contains(&f.id(role)),
            "fully pruned {role} shown in plain tree:\n{t}"
        );
    }
}

#[test]
fn f6_tree_all_marks_pruned_after_status() {
    let (f, _) = migrated();
    let t = f.repo.ok(&["tree", "--all"]).stdout;
    for (role, status, _) in recovered(&f) {
        let l = line_with(&t, &f.id(role)).unwrap_or_else(|| panic!("{role} missing:\n{t}"));
        assert!(
            l.contains(&format!("{status} (pruned)")),
            "{role}: want `{status} (pruned)`:\n{t}"
        );
    }
    let l = line_with(&t, &f.id("ghost")).unwrap();
    assert!(l.contains("done (pruned)"), "ghost:\n{t}");
    for role in ["root", "base", "late"] {
        let l = line_with(&t, &f.id(role)).unwrap_or_else(|| panic!("{role} missing:\n{t}"));
        assert!(!l.contains("(pruned)"), "{role} is not pruned:\n{t}");
    }
    assert!(
        !t.contains(" pruned "),
        "`pruned` still shown as a status:\n{t}"
    );
}

/// F6: a fully pruned subtree is hidden in plain `tree`, as in alpha.1 (fresh repo).
#[test]
fn f6_fully_pruned_subtree_hidden() {
    let r = Repo::init();
    let root = r.run(&["-m", "root"], "true");
    r.write("config.yaml", "lr: 1\n");
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 2\n");
    let b = r.run(&["-m", "b"], "true");
    r.ok(&["fork", &root, "--no-sync"]);
    r.ok(&["prune", &a]);
    let t = r.ok(&["tree"]).stdout;
    assert!(t.contains(&root), "{t}");
    assert!(
        !t.contains(&a) && !t.contains(&b),
        "fully pruned subtree shown:\n{t}"
    );
    let t = r.ok(&["tree", "--all"]).stdout;
    assert!(line_with(&t, &a).unwrap().contains("(pruned)"), "{t}");
    assert!(line_with(&t, &b).unwrap().contains("(pruned)"), "{t}");
}

#[test]
fn f6_siblings_hides_pruned_unless_all() {
    let (f, _) = migrated();
    let mid = f.id("mid");
    // siblings headers show a pin name instead of the id when there is one (M8)
    let labels = |role: &str| match role {
        "best" => vec![f.id(role), "best-v1".into(), "paper".into()],
        _ => vec![f.id(role)],
    };
    let shown = |s: &str, role: &str| labels(role).iter().any(|l| s.contains(l.as_str()));
    let s = f.repo.ok(&["siblings", &mid]).stdout;
    assert!(shown(&s, "late"), "{s}");
    for role in ["best", "crashed", "stopped"] {
        assert!(
            !shown(&s, role),
            "pruned {role} in siblings without --all:\n{s}"
        );
    }
    let s = f.repo.ok(&["siblings", &mid, "--all"]).stdout;
    for role in ["late", "best", "crashed", "stopped"] {
        assert!(shown(&s, role), "{role} missing from siblings --all:\n{s}");
    }
}

// ------------------------------------------------------------------------------------------
// F7. Pruned is a flag in every code path
// ------------------------------------------------------------------------------------------

/// F7 (D-19 unchanged): recipe of a pruned done node → notice + new node; recipe of a live
/// done node → refused without --force.
#[test]
fn f7_duplicate_check_uses_flag() {
    let (f, _) = migrated();
    let r = &f.repo;
    // Same env hash as the fixture (see make.sh: checked-in uv.lock, fixed CUDA string).
    let env = [("POLLARD_CUDA", "none")];
    r.write("config.yaml", "lr: 3\n"); // best's config
    let o = r.po_env(&env, &["run", "--", "sh", "train.sh"]);
    assert!(o.ok(), "rerun of a pruned node's recipe refused:\n{o}");
    assert!(
        o.stderr.contains(&f.id("best")),
        "notice should name best:\n{o}"
    );
    assert!(is_node_id(&o.last_line()), "{o}");
    r.write("config.yaml", "lr: 1\n"); // base's config
    let o = r.po_env(&env, &["run", "--", "sh", "train.sh"]);
    assert!(!o.ok(), "rerun of live done base should be refused:\n{o}");
    assert!(o.stderr.contains(&f.id("base")), "{o}");
}

/// F7: phase-1 prune = whole subtree, sets pruned_at, keeps status; undo clears it.
#[test]
fn f7_prune_sets_flag_keeps_status_undo_clears() {
    let r = Repo::init();
    let root = r.run(&["-m", "root"], "true");
    r.write("config.yaml", "lr: 1\n");
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 2\n");
    let b = r.run_out(&["-m", "b"], "exit 3").last_line();
    assert!(is_node_id(&b));
    r.ok(&["fork", &a, "--no-sync"]);
    r.write("config.yaml", "lr: 3\n");
    let c = r.run(&["-m", "c"], "true");
    r.ok(&["fork", &root, "--no-sync"]);
    r.ok(&["prune", &a]);
    for (id, st) in [(&a, "done"), (&b, "failed"), (&c, "done")] {
        let (s, p) = r.status_of(id);
        assert_eq!(s, st, "{id} status after prune");
        assert!(!p.is_empty(), "{id} pruned_at not set");
    }
    assert_eq!(r.status_of(&root).1, "", "root must not be pruned");
    let s = r.ok(&["show", &b]).stdout;
    let words: Vec<Vec<&str>> = s.lines().map(|l| l.split_whitespace().collect()).collect();
    assert!(words.contains(&vec!["status", "failed"]), "{s}");
    assert!(
        words
            .iter()
            .any(|w| w.first() == Some(&"pruned") && w.len() == 2),
        "{s}"
    );
    r.ok(&["undo"]);
    for id in [&a, &b, &c] {
        assert_eq!(r.status_of(id).1, "", "{id} still pruned after undo");
    }
    assert_eq!(r.status_of(&b).0, "failed");
}

/// F7: an unknown status in the DB is an error naming the node and value, never "pruned".
#[test]
fn f7_bogus_status_in_db_is_an_error() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.sql(&format!("UPDATE nodes SET status='bogus' WHERE id='{a}';"));
    let o = r.fail(&["show", &a]);
    assert!(o.stderr.contains(&a) && o.stderr.contains("bogus"), "{o}");
}

/// F7: no `--json` output carries a `pruned` status value.
#[test]
fn f7_json_has_no_pruned_status() {
    let (f, _) = migrated();
    for role in ["root", "mid", "base"] {
        for extra in [vec![], vec!["--all"]] {
            let mut args = vec!["siblings", "--json"];
            let id = f.id(role);
            args.insert(1, &id);
            args.extend(extra);
            let o = f.repo.ok(&args);
            assert!(!o.stdout.contains("\"pruned\""), "{args:?}:\n{o}");
        }
    }
}

// ------------------------------------------------------------------------------------------
// F8. Pin storage with optional names
// ------------------------------------------------------------------------------------------

fn assert_pins_schema(r: &Repo) {
    let info = r.sql("PRAGMA table_info(pins);");
    let col = |name: &str| {
        info.lines()
            .map(|l| l.split('|').collect::<Vec<_>>())
            .find(|c| c[1] == name)
            .unwrap_or_else(|| panic!("pins has no column {name}:\n{info}"))
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(col("node_id")[3], "1", "node_id must be NOT NULL:\n{info}");
    assert_eq!(col("name")[3], "0", "name must be nullable:\n{info}");
    let ids = node_ids(r);
    let (a, b) = (&ids[0], &ids[ids.len() - 1]);
    let db = r.state_db();
    let ins = |id: &str, name: &str| {
        sql_try(
            &db,
            &format!("INSERT INTO pins(node_id, name) VALUES('{id}', {name});"),
        )
    };
    assert!(ins(a, "NULL").0, "unnamed pin rejected");
    let (ok, e) = ins(a, "NULL");
    assert!(!ok, "a second unnamed pin on one node was accepted: {e}");
    assert!(
        ins(b, "NULL").0 || a == b,
        "unnamed pins on two nodes rejected"
    );
    assert!(ins(a, "'dup-name'").0);
    let (ok, e) = ins(b, "'dup-name'");
    assert!(!ok, "duplicate pin name accepted: {e}");
}

#[test]
fn f8_pins_schema_after_migration() {
    let (f, _) = migrated();
    assert_eq!(
        f.repo
            .sql("SELECT name, node_id FROM pins WHERE name IS NOT NULL ORDER BY name;"),
        golden("pins.txt"),
        "named pins did not carry over"
    );
    assert_pins_schema(&f.repo);
}

#[test]
fn f8_pins_schema_fresh_init() {
    let r = Repo::init();
    r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    r.run(&["-m", "b"], "true");
    assert_pins_schema(&r);
}

/// F8: named-pin behaviour unchanged on a migrated repo (pin, unpin, name as ref, undo).
#[test]
fn f8_named_pins_unchanged_after_migration() {
    let (f, _) = migrated();
    let r = &f.repo;
    let base = f.id("base");
    r.ok(&["pin", &base, "alias"]);
    assert_eq!(
        first_id(&r.ok(&["show", "alias"]).stdout),
        Some(base.clone())
    );
    r.ok(&["undo"]);
    r.fail(&["show", "alias"]);
    r.ok(&["unpin", "paper"]);
    r.fail(&["show", "paper"]);
    assert_eq!(
        first_id(&r.ok(&["show", "best-v1"]).stdout),
        Some(f.id("best")),
        "unpin paper dropped best-v1"
    );
    r.ok(&["undo"]);
    assert_eq!(
        first_id(&r.ok(&["show", "paper"]).stdout),
        Some(f.id("best"))
    );
    let s = r.ok(&["show", &f.id("best")]).stdout;
    assert!(
        s.lines().next().unwrap().contains("[best-v1, paper]"),
        "{s}"
    );
}
