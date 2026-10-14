//! Real generated-file benchmark, intentionally opt-in; no user's files are read.
//! Run: nix develop --command cargo test --test duplicate_performance -- --ignored --nocapture
use file_organizer::{core::FileEntry, duplicates};
use std::{fs, sync::Mutex, time::Instant};

#[test]
#[ignore = "generates 10001 files and measures actual cold/warm I/O"]
fn ten_thousand_file_scan() {
    if let Some(root) = std::env::var_os("DUP_BENCH_ROOT") {
        let root = std::path::PathBuf::from(root);
        fs::create_dir_all(&root).unwrap();
        let mut entries = Vec::new();
        let generated = Instant::now();
        for i in 0..10001u64 {
            let path = root.join(format!("{i:05}.bin"));
            let mut data = vec![0x5a; 65536];
            // Every sample differs; equal sizes alone must not trigger full hashing.
            data[..8].copy_from_slice(&i.to_le_bytes());
            fs::write(&path, data).unwrap();
            entries.push(FileEntry {
                path,
                name: format!("{i:05}.bin"),
                size: 65536,
                dimensions: None,
            });
        }
        println!(
            "generated files={} bytes={} elapsed={:?}",
            entries.len(),
            entries.len() as u64 * 65536,
            generated.elapsed()
        );
        for label in ["cold", "warm"] {
            let last = Mutex::new(None);
            let start = Instant::now();
            let groups =
                duplicates::groups_with_progress(&entries, |p| *last.lock().unwrap() = Some(p))
                    .unwrap();
            let elapsed = start.elapsed();
            let stats = last.into_inner().unwrap().unwrap();
            println!(
                "{label}: files={} elapsed={elapsed:?} bytes_read={} cache_hits={} groups={}",
                entries.len(),
                stats.bytes_read,
                stats.cache_hits,
                groups.len()
            );
            assert!(groups.is_empty());
            if label == "warm" {
                assert_eq!(stats.bytes_read, 0);
                assert_eq!(stats.cache_hits, 10001);
            } else {
                assert!(stats.bytes_read < entries.len() as u64 * 65536);
            }
        }
        return;
    }
    let fixture = tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ten_thousand_file_scan",
            "--ignored",
            "--nocapture",
        ])
        .env("DUP_BENCH_ROOT", fixture.path().join("root"))
        .env("XDG_CACHE_HOME", fixture.path().join("cache"))
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
