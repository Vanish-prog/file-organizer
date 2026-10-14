use file_organizer::{core, duplicates};
use std::{fs, sync::Mutex};

#[test]
fn large_unique_nonimages_do_not_read_full_contents() {
    let d = temp();
    for i in 0..32u8 {
        fs::write(d.path().join(format!("{i:02}.bin")), vec![i; 256 * 1024]).unwrap();
    }
    let events = Mutex::new(Vec::new());
    let result = duplicates::groups_with_progress(&core::scan(d.path()).unwrap(), |p| {
        events.lock().unwrap().push(p)
    })
    .unwrap();
    assert!(result.is_empty());
    let events = events.into_inner().unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.stage, "complete");
    assert_eq!(last.completed, last.total);
    assert!(last.bytes_read < 32 * 32 * 1024, "read {}", last.bytes_read);
}

#[test]
fn sample_matches_are_not_proof_when_unsampled_bytes_differ() {
    let d = temp();
    let mut data = vec![67; 128 * 1024];
    fs::write(d.path().join("a.bin"), &data).unwrap();
    data[16384] = 68;
    fs::write(d.path().join("b.bin"), &data).unwrap();
    let events = Mutex::new(Vec::new());
    let groups = duplicates::groups_with_progress(&core::scan(d.path()).unwrap(), |p| {
        events.lock().unwrap().push(p)
    })
    .unwrap();
    assert!(groups.is_empty());
    assert!(events.into_inner().unwrap().last().unwrap().bytes_read >= 256 * 1024);
}

#[test]
fn persistent_cache_survives_flatten_and_rejects_changed_bytes() {
    if let Some(root) = std::env::var_os("DUP_CACHE_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        fs::create_dir(root.join("nested")).unwrap();
        for name in ["a.bin", "b.bin"] {
            fs::write(root.join("nested").join(name), vec![35; 128 * 1024]).unwrap();
        }
        let run = |directory: &std::path::Path| {
            let events = Mutex::new(Vec::new());
            let groups = duplicates::groups_with_progress(&core::scan(directory).unwrap(), |p| {
                events.lock().unwrap().push(p)
            })
            .unwrap();
            (groups, events.into_inner().unwrap().pop().unwrap())
        };
        let (first, cold) = run(&root.join("nested"));
        assert_eq!(first.len(), 1);
        assert!(cold.bytes_read > 256 * 1024);
        assert_eq!(core::flatten(&root).unwrap(), 2);
        let (second, warm) = run(&root);
        assert_eq!(second.len(), 1);
        assert_eq!(warm.bytes_read, 0);
        assert_eq!(warm.cache_hits, 2);
        fs::write(root.join("b.bin"), vec![36; 128 * 1024]).unwrap();
        let (changed, stats) = run(&root);
        assert!(changed.is_empty());
        assert_eq!(stats.cache_hits, 1);
        assert!(stats.bytes_read > 0);
        return;
    }
    let fixture = temp();
    fs::create_dir(fixture.path().join("root")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "persistent_cache_survives_flatten_and_rejects_changed_bytes",
            "--nocapture",
        ])
        .env("DUP_CACHE_TEST_ROOT", fixture.path().join("root"))
        .env("XDG_CACHE_HOME", fixture.path().join("cache"))
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
fn transient_visual_permission_failure_is_not_cached() {
    isolated_cache(
        "transient_visual_permission_failure_is_not_cached",
        |root, cache| {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("image.png");
            pattern().save(&path).unwrap();
            let entries = core::scan(root).unwrap();
            duplicates::groups_with_progress(&entries, |p| {
                if p.stage == "fingerprints" && p.completed == 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
                }
            })
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(
                fs::read_dir(cache.join("file-organizer/duplicates-v1"))
                    .unwrap()
                    .count(),
                0
            );
            let events = Mutex::new(Vec::new());
            duplicates::groups_with_progress(&entries, |p| events.lock().unwrap().push(p)).unwrap();
            let last = events.into_inner().unwrap().pop().unwrap();
            assert_eq!(last.cache_hits, 0);
            assert!(last.bytes_read > 0);
        },
    );
}

#[test]
fn warm_cache_does_not_rewrite_unchanged_entries() {
    isolated_cache(
        "warm_cache_does_not_rewrite_unchanged_entries",
        |root, cache| {
            pattern().save(root.join("image.png")).unwrap();
            fs::write(root.join("unsupported.bin"), b"not an image").unwrap();
            let mut corrupt = b"\x89PNG\r\n\x1a\n".to_vec();
            corrupt.extend_from_slice(&[0; 64]);
            fs::write(root.join("corrupt.png"), corrupt).unwrap();
            let entries = core::scan(root).unwrap();
            duplicates::groups_with_progress(&entries, |_| {}).unwrap();
            let snapshot = || {
                use std::os::unix::fs::MetadataExt;
                let mut files = fs::read_dir(cache.join("file-organizer/duplicates-v1"))
                    .unwrap()
                    .map(|entry| {
                        let path = entry.unwrap().path();
                        let meta = fs::metadata(&path).unwrap();
                        (
                            path.clone(),
                            meta.ino(),
                            meta.modified().unwrap(),
                            fs::read(path).unwrap(),
                        )
                    })
                    .collect::<Vec<_>>();
                files.sort();
                files
            };
            let cold = snapshot();
            assert_eq!(cold.len(), 3);
            let events = Mutex::new(Vec::new());
            duplicates::groups_with_progress(&entries, |p| events.lock().unwrap().push(p)).unwrap();
            let last = events.into_inner().unwrap().pop().unwrap();
            assert_eq!(last.bytes_read, 0);
            assert_eq!(last.cache_hits, 3);
            assert_eq!(snapshot(), cold, "warm scan replaced unchanged cache files");
        },
    );
}

#[test]
fn cached_entries_are_augmented_when_new_exact_peers_appear() {
    isolated_cache(
        "cached_entries_are_augmented_when_new_exact_peers_appear",
        |root, cache| {
            fs::write(root.join("a.bin"), vec![41; 128 * 1024]).unwrap();
            duplicates::groups_with_progress(&core::scan(root).unwrap(), |_| {}).unwrap();
            let cache_dir = cache.join("file-organizer/duplicates-v1");
            let original = fs::read_dir(&cache_dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let cold: serde_json::Value =
                serde_json::from_slice(&fs::read(&original).unwrap()).unwrap();
            assert!(cold["sample"].is_null());
            assert!(cold["hash"].is_null());
            fs::copy(root.join("a.bin"), root.join("b.bin")).unwrap();
            let entries = core::scan(root).unwrap();
            assert_eq!(
                duplicates::groups_with_progress(&entries, |_| {})
                    .unwrap()
                    .len(),
                1
            );
            let augmented: serde_json::Value =
                serde_json::from_slice(&fs::read(&original).unwrap()).unwrap();
            assert!(augmented["sample"].is_array());
            assert!(augmented["hash"].is_array());
            let events = Mutex::new(Vec::new());
            assert_eq!(
                duplicates::groups_with_progress(&entries, |p| events.lock().unwrap().push(p))
                    .unwrap()
                    .len(),
                1
            );
            let last = events.into_inner().unwrap().pop().unwrap();
            assert_eq!(last.cache_hits, 2);
            assert_eq!(last.bytes_read, 0);
        },
    );
}

fn isolated_cache(name: &str, run: impl FnOnce(&std::path::Path, &std::path::Path)) {
    if let Some(fixture) = std::env::var_os("DUP_CACHE_FIX_ROOT") {
        let fixture = std::path::PathBuf::from(fixture);
        run(&fixture.join("root"), &fixture.join("cache"));
        return;
    }
    let fixture = temp();
    fs::create_dir(fixture.path().join("root")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("DUP_CACHE_FIX_ROOT", fixture.path())
        .env("XDG_CACHE_HOME", fixture.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap()
}

fn pattern() -> image::RgbImage {
    image::RgbImage::from_fn(128, 96, |x, y| {
        let v = ((x * 3 + y * 5 + (x / 20) * 40) % 256) as u8;
        image::Rgb([v, v.saturating_add(10), v.saturating_sub(10)])
    })
}

#[test]
fn groups_require_every_pair_to_match_not_transitive_chains() {
    let d = temp();
    for (name, offset) in [("a.png", 0u8), ("b.png", 7), ("c.png", 14)] {
        image::RgbImage::from_fn(32, 32, |x, y| {
            let v = (60 + ((x * 3 + y * 5) % 120)) as u8 + offset;
            image::Rgb([v, v, v])
        })
        .save(d.path().join(name))
        .unwrap();
    }
    let entries = core::scan(d.path()).unwrap();
    assert_eq!(
        duplicates::find_duplicates(&d.path().join("a.png"), &entries)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        duplicates::find_duplicates(&d.path().join("b.png"), &entries)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        duplicates::groups(&entries).unwrap(),
        vec![vec![d.path().join("a.png"), d.path().join("b.png")]]
    );
}

#[test]
fn perceptual_matches_resized_renamed_image_not_generic_semantics() {
    let d = temp();
    let image = pattern();
    image.save(d.path().join("reference.png")).unwrap();
    image::imageops::resize(&image, 64, 48, image::imageops::FilterType::Triangle)
        .save(d.path().join("renamed.png"))
        .unwrap();
    let inverted = image::RgbImage::from_fn(128, 96, |x, y| {
        let p = image.get_pixel(x, y);
        image::Rgb([255 - p[0], 255 - p[1], 255 - p[2]])
    });
    inverted.save(d.path().join("unrelated.png")).unwrap();
    image::RgbImage::from_pixel(128, 96, image::Rgb([100, 100, 100]))
        .save(d.path().join("flat.png"))
        .unwrap();
    let matches = duplicates::find_duplicates(
        &d.path().join("reference.png"),
        &core::scan(d.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(matches.len(), 1, "{matches:?}");
    assert_eq!(matches[0].path, d.path().join("renamed.png"));
    assert_eq!(matches[0].kind, "perceptual");
    assert!(matches[0].distance <= 8);
}

#[test]
fn exact_duplicates_include_corrupt_images_and_nonimages() {
    let d = temp();
    fs::write(d.path().join("a.png"), b"not a valid image").unwrap();
    fs::write(d.path().join("b.bin"), b"not a valid image").unwrap();
    fs::write(d.path().join("c.png"), b"different").unwrap();
    let entries = core::scan(d.path()).unwrap();
    let matches = duplicates::find_duplicates(&d.path().join("a.png"), &entries).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, d.path().join("b.bin"));
    assert_eq!(matches[0].kind, "exact");
    assert_eq!(matches[0].distance, 0);
}
