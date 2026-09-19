//! Phase 1 upgrade journey (PLAN N1 / N7, BACKLOG X3): an alpha.1 user with a repo and a
//! local remote installs 0.2 → `po tree` (migrates) → `po push` (converts) → a second clone
//! on 0.2 pulls → both `tree --all` identical, and the previously hidden `late` shows in
//! plain `tree` in both.
mod common;
use common::*;

#[test]
fn upgrade_journey_from_alpha1_fixture() {
    let f = Fixture::new();
    let r = &f.repo;
    let late = f.id("late");
    assert!(
        !golden("tree.txt").contains(&late),
        "fixture precondition: late is hidden in alpha.1's plain tree"
    );

    let o = r.ok(&["tree"]);
    assert_eq!(
        o.stderr.matches("upgraded this repo to format 2").count(),
        1,
        "one migration notice:\n{o}"
    );
    assert!(
        o.stdout.contains(&late),
        "late hidden after migration:\n{o}"
    );

    let o = r.ok(&["push"]);
    assert!(
        o.stderr.contains("pollard: converted remote"),
        "no conversion notice:\n{o}"
    );
    assert_eq!(
        std::fs::read_to_string(f.remote.join("FORMAT"))
            .unwrap()
            .trim(),
        "2"
    );

    let second = init_with_remote(&f.remote);
    let o = second.ok(&["pull"]);
    assert!(
        !o.stderr.contains("upgraded this repo"),
        "a fresh 0.2 clone must not migrate:\n{o}"
    );
    assert_eq!(
        strip_at(&second.ok(&["tree", "--all"]).stdout),
        strip_at(&r.ok(&["tree", "--all"]).stdout),
        "tree --all differs between the upgraded repo and the second clone"
    );
    for (name, c) in [("upgraded repo", r), ("second clone", &second)] {
        let t = c.ok(&["tree"]).stdout;
        assert!(t.contains(&late), "{name}: late not in plain tree:\n{t}");
        let l = line_with(&t, &f.id("mid"))
            .unwrap_or_else(|| panic!("{name}: no mid placeholder:\n{t}"));
        assert!(l.contains("(pruned)"), "{name}:\n{t}");
    }
    // Statuses agree too (tree text alone would hide a status mix-up under the same shape).
    let q = "SELECT id, status, coalesce(pruned_at,'') FROM nodes ORDER BY id;";
    assert_eq!(second.sql(q), r.sql(q), "statuses differ between clones");
}
