//! SPEC §10 cross-cutting expectations that span milestones: last-line node ids, undo of every
//! mutating command, and the 200 ms budget on a 1,000-node repo.
mod common;
use common::*;
use std::time::Duration;

#[test]
fn import_last_line_and_undo() {
    let r = Repo::init();
    r.sh(
        "git init -q -b main . && git add -A && git -c user.name=t -c user.email=t@e commit -qm i",
    );
    let o = r.ok(&["import", "HEAD"]);
    let id = o.last_line();
    assert!(is_node_id(&id), "import must print the node id last:\n{o}");
    r.ok(&["undo"]);
    r.fail(&["show", &id]);
}

#[test]
fn fork_undo_restores_current_node() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "b"], "true");
    r.ok(&["fork", &a, "--no-sync"]);
    assert_eq!(r.current(), a);
    r.ok(&["undo"]);
    assert_eq!(
        r.current(),
        b,
        "undo of fork did not restore the current node"
    );
    assert_eq!(
        r.read("config.yaml"),
        "lr: 1\n",
        "undo of fork did not restore the working copy"
    );
}

#[test]
fn undo_of_undo_is_not_required_but_undo_on_empty_log_fails_cleanly() {
    let r = Repo::init();
    let o = r.po(&["undo"]);
    assert!(
        !o.ok() || o.all().to_lowercase().contains("nothing"),
        "undo with empty op log should fail or say nothing to undo:\n{o}"
    );
}

#[test]
fn every_mutating_command_appears_in_op_log() {
    let r = Repo::init();
    let a = r.run(&["-m", "a"], "true");
    r.write("config.yaml", "lr: 1\n");
    let b = r.run(&["-m", "b"], "true");
    r.ok(&["note", &b, "n"]);
    r.ok(&["pin", &b, "p"]);
    r.ok(&["unpin", "p"]);
    r.ok(&["prune", &b]);
    r.ok(&["fork", &a, "--no-sync"]);
    let log = r.ok(&["op", "log"]).stdout;
    for cmd in ["run", "note", "pin", "unpin", "prune", "fork"] {
        assert!(log.contains(cmd), "op log lacks `{cmd}`:\n{log}");
    }
}

/// §10: "No command except run, fork --step, push, pull, and gc takes longer than 200 ms on a
/// 1,000-node repo." Builds the repo through the CLI (~1000 runs), so it is slow.
#[test]
#[ignore = "slow (1000 runs); run with --ignored"]
fn commands_under_200ms_on_1000_node_repo() {
    let r = Repo::init();
    let mut ids = vec![r.run(&["-m", "root"], "emit 1 loss=1")];
    for i in 1..1000 {
        r.write("config.yaml", format!("lr: {i}\n"));
        let parent = ids[(i - 1) / 3].clone();
        ids.push(r.run(
            &["-m", &format!("n{i}"), "--parent", &parent],
            &format!("emit 1 loss={i}"),
        ));
    }
    // Generous-but-real: the spec's 200 ms, measured wall-clock including process start.
    let budget = budget(Duration::from_millis(200)); // spec: 200 ms, release
    let (a, b) = (&ids[500], &ids[501]);
    let checks: Vec<Vec<&str>> = vec![
        vec!["tree"],
        vec!["tree", "--all"],
        vec!["tree", "--metric", "loss"],
        vec!["siblings", &ids[0]],
        vec!["show", a],
        vec!["diff", a, b],
        vec!["log", a, "--key", "loss"],
        vec!["note", a, "renamed"],
        vec!["pin", a, "x"],
        vec!["unpin", "x"],
        vec!["prune", b],
        vec!["undo"],
        vec!["fork", &ids[10], "--no-sync"],
        vec!["op", "log"],
    ];
    let mut slow = vec![];
    for c in &checks {
        let o = r.ok(c);
        if o.elapsed > budget {
            slow.push(format!("{c:?}: {:?}", o.elapsed));
        }
    }
    assert!(
        slow.is_empty(),
        "commands over 200 ms on a 1000-node repo:\n{}",
        slow.join("\n")
    );
}

/// §6: "Target under 50 ms for 100 children" (siblings). Allow process start-up.
#[test]
fn siblings_100_children_fast() {
    let r = Repo::init();
    let p = r.run(&["-m", "P"], "emit 1 loss=1");
    for i in 0..100 {
        r.write("config.yaml", format!("lr: {i}\n"));
        r.run(
            &["-m", &format!("c{i}"), "--parent", &p],
            &format!("emit 1 loss={i}"),
        );
    }
    let o = r.ok(&["siblings", &p]);
    // Spec says 50 ms for the join; 200 ms here covers process start + rendering (generous-but-real).
    assert!(
        o.elapsed < budget(Duration::from_millis(200)),
        "siblings on 100 children took {:?}",
        o.elapsed
    );
}

/// Journey C "Maya kills it": Ctrl-C (SIGINT to the foreground process group) during `run` marks
/// the node `killed`, ingests metrics logged so far, and still prints the node id last.
#[test]
fn ctrl_c_during_run_marks_node_killed() {
    let r = Repo::init();
    let s = r.script(
        "k.sh",
        "emit 1 loss=1; emit 2 loss=2; sleep 0.3; kill -INT 0; sleep 5; emit 3 loss=3",
    );
    let mut c = r.cmd_in(&r.root, &bin(), &["run", "-m", "k", "--", "sh", &s]);
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let o = r.exec(c);
    let id = o.last_line();
    assert!(
        is_node_id(&id),
        "interrupted run must still print its node id last:\n{o}"
    );
    let show = r.ok(&["show", &id]).stdout;
    assert!(
        show.contains("killed"),
        "Ctrl-C'd run should be `killed`:\n{show}"
    );
    let log = r.ok(&["log", &id, "--key", "loss"]).stdout;
    assert!(
        has_num(&log, 2.0) && !log.lines().any(|l| l.trim_start().starts_with('3')),
        "metrics before the kill not ingested (or after):\n{log}"
    );
}
