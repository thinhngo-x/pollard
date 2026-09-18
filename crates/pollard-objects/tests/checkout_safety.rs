use pollard_objects::{Entry, MODE_FILE, MODE_LINK, Manifest, Store, WalkOptions};
use std::{fs, os::unix::fs::symlink};

#[test]
fn unsafe_paths_are_rejected_before_touching_the_working_copy() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("wc");
    fs::create_dir(&root).unwrap();
    let store = Store::new(root.join(".pollard"));
    let hash = store.put_object(b"changed").unwrap();
    let external = t.path().join("outside.txt");
    for path in [
        "../outside.txt".to_owned(),
        external.display().to_string(),
        ".pollard/config.toml".into(),
        ".git/config".into(),
    ] {
        fs::write(root.join("keep.txt"), "keep").unwrap();
        fs::write(&external, "outside").unwrap();
        let manifest = Manifest::new(vec![Entry {
            path,
            hash: hash.clone(),
            size: 7,
            mode: MODE_FILE,
        }]);
        assert!(
            store
                .materialize(&manifest, &root, &WalkOptions::code(&[]))
                .is_err()
        );
        assert_eq!(fs::read_to_string(root.join("keep.txt")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&external).unwrap(), "outside");
    }
}

#[test]
fn symlink_ancestors_cannot_redirect_checkout() {
    for existing in [false, true] {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("wc");
        let outside = t.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("victim"), "outside").unwrap();
        fs::write(root.join("keep"), "keep").unwrap();
        let store = Store::new(root.join(".pollard"));
        let mut entries = vec![Entry {
            path: "link/victim".into(),
            hash: store.put_object(b"changed").unwrap(),
            size: 7,
            mode: MODE_FILE,
        }];
        if existing {
            symlink(&outside, root.join("link")).unwrap();
            fs::write(root.join(".pollardignore"), "link\n").unwrap();
        } else {
            entries.push(Entry {
                path: "link".into(),
                hash: store
                    .put_object(outside.as_os_str().as_encoded_bytes())
                    .unwrap(),
                size: outside.as_os_str().len() as u64,
                mode: MODE_LINK,
            });
        }
        assert!(
            store
                .materialize(&Manifest::new(entries), &root, &WalkOptions::code(&[]))
                .is_err()
        );
        assert_eq!(fs::read_to_string(root.join("keep")).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(outside.join("victim")).unwrap(),
            "outside"
        );
    }
}

#[test]
fn checkout_replaces_a_tracked_symlink_parent_without_following_it() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("wc");
    let outside = t.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), "outside").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    let store = Store::new(root.join(".pollard"));
    let manifest = Manifest::new(vec![Entry {
        path: "link/file".into(),
        hash: store.put_object(b"inside").unwrap(),
        size: 6,
        mode: MODE_FILE,
    }]);
    store
        .materialize(&manifest, &root, &WalkOptions::code(&[]))
        .unwrap();
    assert!(
        !fs::symlink_metadata(root.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(root.join("link/file")).unwrap(),
        "inside"
    );
    assert_eq!(fs::read_to_string(outside.join("file")).unwrap(), "outside");
}

#[test]
fn checkout_round_trips_regular_files_and_leaf_symlinks() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::new(t.path().join(".pollard"));
    fs::write(t.path().join("target"), "target").unwrap();
    fs::write(t.path().join("item"), "regular").unwrap();
    let opts = WalkOptions::code(&[]);
    let regular = store.snapshot_dir(t.path(), &opts).unwrap().manifest;
    fs::remove_file(t.path().join("item")).unwrap();
    symlink("target", t.path().join("item")).unwrap();
    let linked = store.snapshot_dir(t.path(), &opts).unwrap().manifest;
    store.materialize(&regular, t.path(), &opts).unwrap();
    assert_eq!(
        fs::read_to_string(t.path().join("item")).unwrap(),
        "regular"
    );
    store.materialize(&linked, t.path(), &opts).unwrap();
    assert_eq!(
        fs::read_link(t.path().join("item")).unwrap().to_str(),
        Some("target")
    );
    assert_eq!(
        fs::read_to_string(t.path().join("target")).unwrap(),
        "target"
    );
}
