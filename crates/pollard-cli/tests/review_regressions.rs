mod common;
use common::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn failed_blob_push_can_be_retried() {
    let remote = tempfile::tempdir().unwrap();
    let r = Repo::init();
    r.set_config(
        "remote",
        &format!("{:?}", remote.path().display().to_string()),
    );
    let id = r.run(&[], "true");
    fs::write(remote.path().join("objects"), "block upload").unwrap();
    r.fail(&["push"]);
    fs::remove_file(remote.path().join("objects")).unwrap();
    r.ok(&["push"]);
    // format 2: node lines live in per-push segments under nodes/
    let segs = fs::read_dir(remote.path().join("nodes")).unwrap();
    assert!(
        segs.flatten()
            .any(|e| fs::read_to_string(e.path()).unwrap().contains(&id))
    );
    let clone = Repo::init();
    clone.set_config(
        "remote",
        &format!("{:?}", remote.path().display().to_string()),
    );
    clone.ok(&["pull"]);
    clone.ok(&["fork", &id, "--no-sync"]);
}

#[test]
fn undo_note_preserves_subsequent_run_completion_and_sdk_config() {
    let r = Repo::init();
    r.set_config("config_capture", "\"sdk\"");
    let id = r.run(&["-m", "original"], "pollard note @ edited; printf '{\"lr\":42}' > \"$POLLARD_CONFIG\"; printf weights > \"$POLLARD_CKPT_DIR/step10.pt\"");
    let repo = pollard_core::Repo::discover(&r.root).unwrap();
    let completed = repo.node(&id).unwrap();
    assert_eq!(completed.status, pollard_core::Status::Done);
    assert!(completed.weights.is_some());
    r.ok(&["undo"]);
    let restored = repo.node(&id).unwrap();
    assert_eq!(restored.note.as_deref(), Some("original"));
    assert_eq!(restored.status, completed.status);
    assert_eq!(restored.finished_at, completed.finished_at);
    assert_eq!(restored.weights, completed.weights);
    assert_eq!(restored.config, completed.config);
    assert_eq!(restored.recipe_hash, completed.recipe_hash);
    r.ok(&["undo"]);
    assert_eq!(repo.node(&id).unwrap(), completed);
}

#[test]
fn apply_permission_only_change_preserves_local_contents_and_undo_restores_mode() {
    for (before, after) in [(0o644, 0o755), (0o755, 0o644)] {
        let r = Repo::init();
        r.write("script.sh", "echo base\n");
        fs::set_permissions(r.root.join("script.sh"), fs::Permissions::from_mode(before)).unwrap();
        let parent = r.run(&[], "true");
        fs::set_permissions(r.root.join("script.sh"), fs::Permissions::from_mode(after)).unwrap();
        let child = r.run(&[], "true");
        r.ok(&["fork", &parent, "--no-sync"]);
        r.ok(&["apply", &child]);
        assert_eq!(
            fs::metadata(r.root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            after
        );
        r.ok(&["undo"]);
        assert_eq!(
            fs::metadata(r.root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            before
        );
        r.write("script.sh", "echo local\n");
        r.ok(&["apply", &child]);
        assert_eq!(r.read("script.sh"), "echo local\n");
        assert_eq!(
            fs::metadata(r.root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            after
        );
    }
}

#[test]
fn apply_symlink_delta_replaces_link_without_modifying_target() {
    let r = Repo::init();
    r.write("a.txt", "a\n");
    r.write("b.txt", "b\n");
    symlink("a.txt", r.root.join("link")).unwrap();
    let parent = r.run(&[], "true");
    r.rm("link");
    symlink("b.txt", r.root.join("link")).unwrap();
    let child = r.run(&[], "true");
    r.ok(&["fork", &parent, "--no-sync"]);
    r.ok(&["apply", &child]);
    assert_eq!(
        fs::read_link(r.root.join("link")).unwrap().to_str(),
        Some("b.txt")
    );
    assert_eq!(r.read("a.txt"), "a\n");
    assert_eq!(r.read("b.txt"), "b\n");
}

#[test]
fn captured_offtree_config_is_restored_by_fork_apply_and_undo() {
    let r = Repo::init();
    r.set_config("config_capture", "\"file:notes/config.yaml\"");
    r.write("notes/config.yaml", "lr: 1\n");
    r.write("notes/idea.txt", "initial\n");
    let parent = r.run(&[], "true");
    r.write("notes/config.yaml", "lr: 2\n");
    let child = r.run(&[], "true");
    r.write("notes/idea.txt", "keep local\n");
    r.ok(&["fork", &parent, "--no-sync"]);
    assert_eq!(r.read("notes/config.yaml"), "lr: 1\n");
    r.ok(&["undo"]);
    assert_eq!(r.read("notes/config.yaml"), "lr: 2\n");
    r.ok(&["fork", &parent, "--no-sync"]);
    r.ok(&["apply", &child]);
    assert_eq!(r.read("notes/config.yaml"), "lr: 2\n");
    assert_eq!(r.read("notes/idea.txt"), "keep local\n");
}
