use file_organizer::core;
use std::{fs, path::Path};

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::var("TMPDIR").expect("scratch TMPDIR")).unwrap()
}

#[cfg(unix)]
#[test]
fn root_symlinks_are_rejected_even_with_trailing_separator() {
    let d = temp();
    let actual = temp();
    std::os::unix::fs::symlink(actual.path(), d.path().join("root-link")).unwrap();
    let trailing = std::path::PathBuf::from(format!("{}/root-link/", d.path().display()));
    assert!(core::scan(&trailing).is_err());
    assert!(core::flatten(&trailing).is_err());
}

#[test]
fn root_file_and_traversal_inputs_are_rejected_without_changes() {
    let d = temp();
    let source = d.path().join("a.txt");
    fs::write(&source, b"safe").unwrap();
    fs::create_dir(d.path().join("nested")).unwrap();
    fs::write(d.path().join("nested/b.txt"), b"nested").unwrap();
    for bad in [
        d.path().to_path_buf(),
        d.path().join("nested/b.txt"),
        d.path().join("nested/../a.txt"),
    ] {
        assert!(core::include(d.path(), "target", &[source.clone(), bad.clone()]).is_err());
        assert!(core::merge_to_trash(d.path(), &source, &[bad]).is_err());
        assert!(source.exists());
        assert!(!d.path().join("target").exists());
    }
    assert!(core::scan(&source).is_err());
    assert!(core::flatten(&source).is_err());
    assert_eq!(core::include(d.path(), "unused", &[]).unwrap(), 0);
    assert!(!d.path().join("unused").exists());
}

#[cfg(unix)]
#[test]
fn flatten_never_traverses_directory_links_or_overwrites_dangling_links() {
    let d = temp();
    let external = temp();
    fs::write(external.path().join("untouched"), b"outside").unwrap();
    std::os::unix::fs::symlink(external.path(), d.path().join("linked-dir")).unwrap();
    std::os::unix::fs::symlink("/missing", d.path().join("a.txt")).unwrap();
    fs::create_dir(d.path().join("nested")).unwrap();
    fs::write(d.path().join("nested/a.txt"), b"inside").unwrap();
    assert_eq!(core::flatten(d.path()).unwrap(), 1);
    assert_eq!(
        fs::read(external.path().join("untouched")).unwrap(),
        b"outside"
    );
    assert!(
        fs::symlink_metadata(d.path().join("a.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(d.path().join("a (1).txt")).unwrap(), b"inside");
    assert!(core::scan(&d.path().join("linked-dir")).is_err());
    assert!(
        core::merge_to_trash(
            d.path(),
            &d.path().join("a (1).txt"),
            &[d.path().join("a.txt")]
        )
        .is_err()
    );
}

#[test]
fn merge_trashes_only_validated_others_in_isolated_subprocess() {
    if std::env::var_os("ORGANIZER_TRASH_CHILD").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("ORGANIZER_TEST_ROOT").unwrap());
        let master = root.join("master.txt");
        let other = root.join("other.txt");
        fs::write(&master, b"master").unwrap();
        fs::write(&other, b"other").unwrap();
        assert!(
            core::merge_to_trash(&root, &master, &[other.clone(), root.join("missing")]).is_err()
        );
        assert!(other.exists());
        assert!(core::merge_to_trash(&root, &master, std::slice::from_ref(&master)).is_err());
        assert!(core::merge_to_trash(&root, &master, &[other.clone(), other.clone()]).is_err());
        assert_eq!(
            core::merge_to_trash(&root, &master, std::slice::from_ref(&other)).unwrap(),
            1
        );
        assert_eq!(fs::read(master).unwrap(), b"master");
        assert!(!other.exists());
        let data = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap());
        assert_eq!(fs::read_dir(data.join("Trash/files")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(data.join("Trash/info")).unwrap().count(), 1);
        return;
    }
    let d = temp();
    fs::create_dir(d.path().join("root")).unwrap();
    fs::create_dir(d.path().join("data")).unwrap();
    fs::create_dir(d.path().join("home")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "merge_trashes_only_validated_others_in_isolated_subprocess",
            "--nocapture",
        ])
        .env("ORGANIZER_TRASH_CHILD", "1")
        .env("ORGANIZER_TEST_ROOT", d.path().join("root"))
        .env("XDG_DATA_HOME", d.path().join("data"))
        .env("HOME", d.path().join("home"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn include_preflights_all_paths_and_never_overwrites() {
    let d = temp();
    let outside = temp();
    let a = d.path().join("a.txt");
    fs::write(&a, b"new").unwrap();
    let foreign = outside.path().join("foreign.txt");
    fs::write(&foreign, b"foreign").unwrap();
    for folder in ["", ".", "..", "../escape", "a/b", "/absolute", "a\\b"] {
        assert!(
            core::include(d.path(), folder, std::slice::from_ref(&a)).is_err(),
            "{folder}"
        );
        assert!(a.exists());
    }
    assert!(core::include(d.path(), "out", &[a.clone(), foreign]).is_err());
    assert!(!d.path().join("out").exists());
    assert!(core::include(d.path(), "out", &[a.clone(), a.clone()]).is_err());
    fs::create_dir(d.path().join("out")).unwrap();
    fs::write(d.path().join("out/a.txt"), b"old").unwrap();
    assert_eq!(
        core::include(d.path(), "out", std::slice::from_ref(&a)).unwrap(),
        1
    );
    assert!(!a.exists());
    assert_eq!(fs::read(d.path().join("out/a.txt")).unwrap(), b"old");
    assert_eq!(fs::read(d.path().join("out/a (1).txt")).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn include_rejects_symlink_source_and_target() {
    let d = temp();
    let outside = temp();
    fs::write(d.path().join("a"), b"a").unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("out")).unwrap();
    assert!(core::include(d.path(), "out", &[d.path().join("a")]).is_err());
    std::os::unix::fs::symlink(d.path().join("a"), d.path().join("link")).unwrap();
    assert!(
        core::include(
            d.path(),
            "safe",
            &[d.path().join("a"), d.path().join("link")]
        )
        .is_err()
    );
    assert!(d.path().join("a").exists());
    assert!(!d.path().join("safe").exists());
}

#[test]
fn flatten_keeps_collisions_and_symlinks_and_removes_only_empty_dirs() {
    let d = temp();
    fs::write(d.path().join("a.txt"), b"root").unwrap();
    fs::create_dir_all(d.path().join("nested/deep")).unwrap();
    fs::write(d.path().join("nested/deep/a.txt"), b"deep").unwrap();
    fs::write(d.path().join("nested/b.txt"), b"other").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/outside/missing", d.path().join("nested/link")).unwrap();
    assert_eq!(core::flatten(d.path()).unwrap(), 2);
    assert_eq!(fs::read(d.path().join("a.txt")).unwrap(), b"root");
    assert_eq!(fs::read(d.path().join("a (1).txt")).unwrap(), b"deep");
    assert_eq!(fs::read(d.path().join("b.txt")).unwrap(), b"other");
    assert!(!d.path().join("nested/deep").exists());
    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(d.path().join("nested/link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(core::flatten(d.path()).unwrap(), 0);
}

#[test]
fn scan_only_direct_regular_files_with_image_dimensions() {
    let d = temp();
    fs::write(d.path().join("z.txt"), b"hello").unwrap();
    fs::create_dir(d.path().join("nested")).unwrap();
    fs::write(d.path().join("nested/hidden.txt"), b"nested").unwrap();
    image::GrayImage::new(20, 30)
        .save(d.path().join("a.png"))
        .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("z.txt"), d.path().join("link")).unwrap();
    let entries = core::scan(d.path()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "a.png");
    assert_eq!(entries[0].dimensions, Some((20, 30)));
    assert_eq!(entries[1].size, 5);
    assert!(entries.iter().all(|e| e.path.is_absolute()));
    assert!(core::scan(Path::new("/definitely/missing/root")).is_err());
}
