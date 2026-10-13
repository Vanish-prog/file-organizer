use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub root: PathBuf,
    pub query: String,
    pub reference: Option<PathBuf>,
    pub limit: usize,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Hit {
    pub path: PathBuf,
    pub score: f64,
}
fn parse_hits(root: &std::path::Path, data: &[u8]) -> Result<Vec<Hit>> {
    let root = root.canonicalize()?;
    let mut hits: Vec<Hit> = serde_json::from_slice(data)?;
    for hit in &hits {
        anyhow::ensure!(
            hit.score.is_finite(),
            "findmatch returned a non-finite score"
        );
        anyhow::ensure!(
            hit.path.is_absolute()
                && !hit
                    .path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "findmatch returned an unsafe path"
        );
    }
    hits.retain(|hit| {
        hit.path.parent() == Some(root.as_path())
            && std::fs::symlink_metadata(&hit.path).is_ok_and(|m| m.file_type().is_file())
    });
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(hits)
}
fn permission_response(prompt: &str, processing: bool, download: bool) -> Option<&'static str> {
    if prompt.contains("Process these files before searching? [y/N]") {
        Some(if processing { "y\n" } else { "n\n" })
    } else if prompt.contains("Download EmbeddingGemma 2") && prompt.contains("[y/N]") {
        Some(if download { "y\n" } else { "n\n" })
    } else if prompt.contains("[y/N]") {
        Some("n\n")
    } else {
        None
    }
}

fn cached_model_available(cache: &std::path::Path) -> bool {
    cache
        .join("findmatch/models/embeddinggemma-2-740m.litertlm")
        .is_file()
}
/// A cache-presence check, not an integrity or inference check; findmatch validates usage.
pub fn model_available() -> bool {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
    cache.as_deref().is_some_and(cached_model_available)
}
/// Conservative: confirm processing for every valid root. findmatch alone checks its
/// index fingerprints and will skip its prompt when files are unchanged.
pub fn needs_processing(root: &std::path::Path) -> Result<bool> {
    anyhow::ensure!(
        root.canonicalize()?.is_dir(),
        "Search root is not a directory"
    );
    Ok(true)
}

fn resolve_program(runtime: Option<std::ffi::OsString>, compiled: Option<&str>) -> PathBuf {
    runtime
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(compiled.unwrap_or("findmatch")))
}

pub fn search(
    req: &SearchRequest,
    allow_processing: bool,
    allow_download: bool,
) -> Result<Vec<Hit>> {
    let program = resolve_program(
        std::env::var_os("FILE_ORGANIZER_FINDMATCH"),
        option_env!("FILE_ORGANIZER_FINDMATCH"),
    );
    run_search(
        req,
        allow_processing,
        allow_download,
        &program,
        std::time::Duration::from_secs(600),
    )
}
fn run_search(
    req: &SearchRequest,
    processing: bool,
    download: bool,
    program: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<Vec<Hit>> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    anyhow::ensure!(req.limit > 0, "Search limit must be positive");
    let root = req.root.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "Search root is not a directory");
    let mut command = Command::new(program);
    command
        .arg("--json")
        .arg("--limit")
        .arg(req.limit.to_string())
        .arg(&root);
    if let Some(reference) = &req.reference {
        command.arg("--reference").arg(reference.canonicalize()?);
    }
    command.arg("--").arg(&req.query);
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || -> std::io::Result<(Vec<u8>, bool)> {
        let mut data = Vec::new();
        let mut buffer = [0; 8192];
        let mut overflow = false;
        loop {
            let n = stdout.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            let remaining = (8 * 1024 * 1024usize).saturating_sub(data.len());
            data.extend_from_slice(&buffer[..n.min(remaining)]);
            overflow |= n > remaining;
        }
        Ok((data, overflow))
    });
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let err = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut diagnostics = std::collections::VecDeque::new();
        let mut line = std::collections::VecDeque::new();
        let mut byte = [0];
        while stderr.read(&mut byte)? != 0 {
            if diagnostics.len() == 16384 {
                diagnostics.pop_front();
            }
            diagnostics.push_back(byte[0]);
            if line.len() == 4096 {
                line.pop_front();
            }
            line.push_back(byte[0]);
            if line
                .iter()
                .rev()
                .take(5)
                .copied()
                .eq(b"]N/y[".iter().copied())
            {
                let prompt: Vec<_> = line.iter().copied().collect();
                let answer =
                    permission_response(&String::from_utf8_lossy(&prompt), processing, download)
                        .unwrap_or("n\n");
                stdin.write_all(answer.as_bytes())?;
                stdin.flush()?;
                line.clear();
            } else if byte[0] == b'\n' {
                line.clear();
            }
        }
        Ok(diagnostics.into_iter().collect())
    });
    let start = std::time::Instant::now();
    let mut status = None;
    let mut failure = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => status = Some(s),
            Ok(None) => {}
            Err(e) => {
                failure = Some(format!("Cannot wait for findmatch: {e}"));
                break;
            }
        }
        if status.is_some() && out.is_finished() && err.is_finished() {
            break;
        }
        if start.elapsed() >= timeout {
            failure = Some("findmatch timed out".to_string());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if failure.is_some() {
        // Kill the process group too: inherited pipes otherwise keep readers alive.
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        unsafe {
            kill(-(child.id() as i32), 9);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let output = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader failed"))?;
    let diagnostic_result = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))?;
    if let Some(failure) = failure {
        anyhow::bail!("{failure}");
    }
    let (data, overflow) = output?;
    let diagnostics = diagnostic_result?;
    let status = status.unwrap();
    anyhow::ensure!(!overflow, "findmatch output exceeds 8 MiB");
    anyhow::ensure!(
        status.success(),
        "findmatch failed ({status}): {}",
        String::from_utf8_lossy(&diagnostics)
    );
    let mut hits = parse_hits(&root, &data)?;
    hits.truncate(req.limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_findmatch_does_not_depend_on_path() {
        assert_eq!(
            resolve_program(None, Some("/nix/store/pinned-findmatch/bin/findmatch")),
            PathBuf::from("/nix/store/pinned-findmatch/bin/findmatch")
        );
    }
    #[test]
    fn deliberate_runtime_override_wins() {
        assert_eq!(
            resolve_program(
                Some("/fixture/findmatch".into()),
                Some("/nix/store/pinned-findmatch/bin/findmatch")
            ),
            PathBuf::from("/fixture/findmatch")
        );
        assert_eq!(resolve_program(None, None), PathBuf::from("findmatch"));
    }
    // Concurrent fork can briefly inherit another test's writable script fd,
    // producing Linux ETXTBSY despite close-on-exec. Serialize fixture lifetimes.
    static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct FixtureDirectory {
        _directory: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    fn fixture(body: &str) -> (FixtureDirectory, SearchRequest, PathBuf) {
        let guard = FIXTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a"), "a").unwrap();
        let program = dir.path().join("protocol fixture");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        (
            FixtureDirectory {
                _directory: dir,
                _guard: guard,
            },
            SearchRequest {
                root,
                query: "a; touch NEVER".into(),
                reference: None,
                limit: 10,
            },
            program,
        )
    }
    #[test]
    fn subprocess_answers_processing_prompt_without_newline() {
        let (_dir, req, program) = fixture(
            "printf 'Process these files before searching? [y/N]' >&2\nread answer\n[ \"$answer\" = y ] || exit 2\nprintf '[{\"path\":\"%s/a\",\"score\":0.8}]' \"$4\"",
        );
        let hits = run_search(
            &req,
            true,
            false,
            &program,
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn unknown_prompt_is_denied_even_with_both_permissions() {
        assert_eq!(
            permission_response("Erase everything? [y/N]", true, true),
            Some("n\n")
        );
    }
    #[test]
    fn download_requires_separate_permission() {
        let (_dir, req, program) = fixture(
            "printf 'Download EmbeddingGemma 2 (approximately 485 MB) for local inference? [y/N]' >&2\nread answer\n[ \"$answer\" = n ] || exit 8\nprintf '[]'",
        );
        assert!(
            run_search(
                &req,
                true,
                false,
                &program,
                std::time::Duration::from_secs(2)
            )
            .is_ok()
        );
    }
    #[test]
    fn processing_denial_returns_cancellation_error() {
        let (_dir, req, program) = fixture(
            "printf 'Process these files before searching? [y/N]' >&2\nread answer\n[ \"$answer\" = n ] || exit 8\nprintf 'Search cancelled; no files processed.' >&2\nexit 2",
        );
        let err = run_search(
            &req,
            false,
            true,
            &program,
            std::time::Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err:#}");
    }
    #[test]
    fn timeout_kills_child_and_reaps_readers() {
        let (_dir, req, program) = fixture("printf '%s' \"$$\" > \"$4/pid\"\nsleep 1\nprintf '[]'");
        let start = std::time::Instant::now();
        let result = run_search(
            &req,
            true,
            false,
            &program,
            std::time::Duration::from_millis(50),
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < std::time::Duration::from_millis(900));
        let pid = std::fs::read_to_string(req.root.join("pid")).unwrap();
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    }
    #[test]
    fn diagnostics_are_bounded_but_late_prompts_still_work() {
        let (_dir, req, program) = fixture(
            "head -c 100000 /dev/zero | tr '\\000' x >&2\nprintf '\\nProcess these files before searching? [y/N]' >&2\nread answer\n[ \"$answer\" = y ] || exit 8\nexit 2",
        );
        let message = run_search(
            &req,
            true,
            false,
            &program,
            std::time::Duration::from_secs(2),
        )
        .unwrap_err()
        .to_string();
        assert!(message.len() < 20000);
        assert!(message.contains("exit status: 2"));
    }
    #[test]
    fn stdout_is_bounded_and_drained() {
        let (_dir, req, program) = fixture("head -c 9000000 /dev/zero\nprintf 'done' >&2");
        let message = run_search(
            &req,
            false,
            false,
            &program,
            std::time::Duration::from_secs(2),
        )
        .unwrap_err()
        .to_string();
        assert!(message.contains("output exceeds"));
    }
    #[test]
    fn processing_check_is_conservative_and_validates_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(needs_processing(dir.path()).unwrap());
        assert!(needs_processing(&dir.path().join("missing")).is_err());
    }
    #[test]
    fn detects_model_in_findmatch_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("findmatch/models/embeddinggemma-2-740m.litertlm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "cached model fixture").unwrap();
        assert!(cached_model_available(dir.path()));
    }
    #[test]
    #[ignore = "requires installed findmatch and existing cached model; never grants download"]
    fn real_findmatch_cached_model_on_demo_copies() {
        assert!(
            model_available(),
            "cached model required; downloading is forbidden in this test"
        );
        let demo = PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("projects/search-tool/examples/demo");
        let dir = tempfile::tempdir().unwrap();
        for entry in std::fs::read_dir(demo).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
            }
        }
        let req = SearchRequest {
            root: dir.path().to_path_buf(),
            query: "radio frequency antenna calibration".into(),
            reference: None,
            limit: 5,
        };
        let hits = search(&req, true, false).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path.file_name().unwrap(), "rf.txt");
        for hit in &hits {
            println!("REAL TEXT {:.6} {}", hit.score, hit.path.display());
        }
        let image_req = SearchRequest {
            query: String::new(),
            reference: Some(dir.path().join("circle.png")),
            ..req
        };
        let image_hits = search(&image_req, false, false).unwrap();
        assert!(!image_hits.is_empty());
        for hit in image_hits {
            println!("REAL REFERENCE {:.6} {}", hit.score, hit.path.display());
        }
    }
    #[test]
    fn query_and_reference_are_literal_arguments() {
        let (_dir, mut req, program) = fixture(
            "[ \"$1\" = --json ] && [ \"$2\" = --limit ] && [ \"$5\" = --reference ] && [ \"$7\" = -- ] && [ \"$8\" = 'a; touch NEVER' ] || exit 7\nprintf '[]'",
        );
        req.reference = Some(req.root.join("a"));
        assert!(
            run_search(
                &req,
                false,
                false,
                &program,
                std::time::Duration::from_secs(2)
            )
            .is_ok()
        );
    }
    #[test]
    fn stdout_is_drained_before_prompt_response() {
        let (_dir, req, program) = fixture(
            "head -c 200000 /dev/zero | tr '\\000' ' '\nprintf 'Process these files before searching? [y/N]' >&2\nread answer\n[ \"$answer\" = y ] || exit 8\nprintf '[]'",
        );
        assert!(
            run_search(
                &req,
                true,
                false,
                &program,
                std::time::Duration::from_secs(2)
            )
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn parses_ranked_direct_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a"), "a").unwrap();
        std::fs::write(root.join("b"), "b").unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/c"), "c").unwrap();
        let data = serde_json::to_vec(&serde_json::json!([
 {"path":root.join("a"),"score":0.2},{"path":root.join("nested/c"),"score":0.99},
 {"path":root.join("b"),"score":0.8}]))
        .unwrap();
        let hits = parse_hits(root, &data).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, root.join("b"));
    }
    #[test]
    fn rejects_traversal_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "a").unwrap();
        let data=serde_json::to_vec(&serde_json::json!([{"path":dir.path().join("../").join(dir.path().file_name().unwrap()).join("a"),"score":0.8}])).unwrap();
        assert!(parse_hits(dir.path(), &data).is_err());
    }
    #[test]
    fn rejects_malformed_scores_and_json() {
        let dir = tempfile::tempdir().unwrap();
        for data in [
            b"not json".as_slice(),
            b"[{\"path\":\"/a\",\"score\":NaN}]",
            b"[{\"path\":\"/a\",\"score\":1e999}]",
        ] {
            assert!(parse_hits(dir.path(), data).is_err());
        }
    }
    #[test]
    fn skips_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let data = serde_json::to_vec(
            &serde_json::json!([{"path":dir.path().join("missing"),"score":0.8}]),
        )
        .unwrap();
        assert!(parse_hits(dir.path(), &data).unwrap().is_empty());
    }
    #[test]
    fn processing_requires_explicit_permission() {
        assert_eq!(
            permission_response(
                "3 files need processing. Process these files before searching? [y/N]",
                true,
                false
            ),
            Some("y\n")
        );
        assert_eq!(
            permission_response("Process these files before searching? [y/N]", false, true),
            Some("n\n")
        );
    }
}
