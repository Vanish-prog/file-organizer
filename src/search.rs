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
/// File-start events report N-1 completed files; completion is emitted only after
/// successful, valid backend output. The current backend exposes only indexing
/// counts, not a reliable ranking/model-loading stage boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchProgress {
    pub stage: String,
    pub completed: usize,
    pub total: usize,
}

fn report_progress(progress: &dyn Fn(SearchProgress), event: SearchProgress) {
    // An observer must not unwind through the pipe reader or strand the child.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| progress(event)));
}

fn processing_progress(line: &str) -> Option<SearchProgress> {
    let (counts, filename) = line.strip_prefix("Processing ")?.split_once(": ")?;
    if filename.is_empty() {
        return None;
    }
    let (number, total) = counts.split_once('/')?;
    if number.is_empty()
        || total.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
        || !total.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let number: usize = number.parse().ok()?;
    let total: usize = total.parse().ok()?;
    if number == 0 || total == 0 || number > total {
        return None;
    }
    Some(SearchProgress {
        stage: "indexing".into(),
        completed: number - 1,
        total,
    })
}

fn observe_processing(
    line: &[u8],
    last: &mut Option<SearchProgress>,
    progress: &dyn Fn(SearchProgress),
) {
    if let Some(event) = processing_progress(&String::from_utf8_lossy(line)) {
        if last.as_ref().is_some_and(|previous| {
            previous.total != event.total || previous.completed >= event.completed
        }) {
            return;
        }
        report_progress(progress, event.clone());
        *last = Some(event);
    }
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
    // Filenames and backend diagnostics are data even when they contain [y/N].
    if prompt.starts_with("Processing ") || prompt.starts_with("findmatch:") {
        return None;
    }
    let processing_prompt = "Process these files before searching? [y/N]";
    let numbered_prompt = prompt
        .strip_suffix(processing_prompt)
        .and_then(|s| s.strip_suffix(" files need processing. "))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if prompt == processing_prompt || numbered_prompt {
        Some(if processing { "y\n" } else { "n\n" })
    } else if prompt
        == "Download EmbeddingGemma 2 (approximately 485 MB) for local inference? [y/N]"
    {
        Some(if download { "y\n" } else { "n\n" })
    } else if prompt.ends_with("[y/N]") {
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
    search_with_progress(req, allow_processing, allow_download, |_| {})
}

/// Streams bounded stderr indexing records as they arrive. Observers run on the
/// stderr reader thread (final completion on the caller) and should return promptly;
/// observer panics are caught. Cached searches may produce no progress events.
pub fn search_with_progress(
    req: &SearchRequest,
    allow_processing: bool,
    allow_download: bool,
    progress: impl Fn(SearchProgress) + Send + Sync + 'static,
) -> Result<Vec<Hit>> {
    let program = resolve_program(
        std::env::var_os("FILE_ORGANIZER_FINDMATCH"),
        option_env!("FILE_ORGANIZER_FINDMATCH"),
    );
    run_search_with_progress(
        req,
        allow_processing,
        allow_download,
        &program,
        std::time::Duration::from_secs(600),
        progress,
    )
}
#[cfg(test)]
fn run_search(
    req: &SearchRequest,
    processing: bool,
    download: bool,
    program: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<Vec<Hit>> {
    run_search_with_progress(req, processing, download, program, timeout, |_| {})
}

fn run_search_with_progress(
    req: &SearchRequest,
    processing: bool,
    download: bool,
    program: &std::path::Path,
    timeout: std::time::Duration,
    progress: impl Fn(SearchProgress) + Send + Sync + 'static,
) -> Result<Vec<Hit>> {
    let progress = std::sync::Arc::new(progress);
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    anyhow::ensure!(req.limit > 0, "Search limit must be positive");
    let root = req.root.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "Search root is not a directory");
    // Legacy findmatch prints filenames verbatim. CR/LF filenames make its
    // human-readable protocol ambiguous; retain an indeterminate UI for those trees.
    let unambiguous_progress = walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .all(|entry| {
            entry.is_ok_and(|entry| {
                use std::os::unix::ffi::OsStrExt;
                !entry
                    .file_name()
                    .as_bytes()
                    .iter()
                    .any(|b| *b == b'\n' || *b == b'\r')
            })
        });
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
    let reader_progress = progress.clone();
    let err = std::thread::spawn(
        move || -> std::io::Result<(Vec<u8>, Option<SearchProgress>)> {
            let mut diagnostics = std::collections::VecDeque::new();
            let mut line = Vec::new();
            let mut truncated = false;
            let mut prompt_space = false;
            let mut last_progress = None;
            let mut started_processing = false;
            let mut byte = [0];
            while stderr.read(&mut byte)? != 0 {
                if diagnostics.len() == 16384 {
                    diagnostics.pop_front();
                }
                diagnostics.push_back(byte[0]);
                if prompt_space {
                    prompt_space = false;
                    if byte[0] == b' ' {
                        continue;
                    }
                }
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    started_processing |= line.starts_with(b"Processing ");
                    if unambiguous_progress {
                        observe_processing(&line, &mut last_progress, reader_progress.as_ref());
                    }
                    line.clear();
                    truncated = false;
                    continue;
                }
                // Preserve the anchored prefix, never reinterpret a truncated tail.
                if line.len() < 4096 {
                    line.push(byte[0]);
                } else {
                    truncated = true;
                }
                if !started_processing
                    && !truncated
                    && line.ends_with(b"[y/N]")
                    && let Some(answer) =
                        permission_response(&String::from_utf8_lossy(&line), processing, download)
                {
                    stdin.write_all(answer.as_bytes())?;
                    stdin.flush()?;
                    line.clear();
                    prompt_space = true; // ask() prints one trailing space.
                }
            }
            if unambiguous_progress {
                observe_processing(&line, &mut last_progress, reader_progress.as_ref());
            }
            Ok((diagnostics.into_iter().collect(), last_progress))
        },
    );
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
    let (diagnostics, last_progress) = diagnostic_result?;
    let status = status.unwrap();
    anyhow::ensure!(!overflow, "findmatch output exceeds 8 MiB");
    anyhow::ensure!(
        status.success(),
        "findmatch failed ({status}): {}",
        String::from_utf8_lossy(&diagnostics)
    );
    let mut hits = parse_hits(&root, &data)?;
    hits.truncate(req.limit);
    if let Some(mut event) = last_progress {
        event.completed = event.total;
        report_progress(progress.as_ref(), event);
    }
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
    fn progress_streams_before_exit_and_completes_only_after_success() {
        let (_dir, req, program) = fixture(
            "printf 'Processing 1/2: a\\rProcessing 2/2: b\\n' >&2\nwhile [ ! -f \"$4/observed\" ]; do sleep 0.01; done\nprintf '[]'",
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = events.clone();
        let marker = req.root.join("observed");
        run_search_with_progress(
            &req,
            true,
            false,
            &program,
            std::time::Duration::from_secs(2),
            move |event: SearchProgress| {
                if event.completed == 1 {
                    std::fs::write(&marker, "observed while backend running").unwrap();
                }
                collected.lock().unwrap().push(event);
            },
        )
        .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| (e.stage.as_str(), e.completed, e.total))
                .collect::<Vec<_>>(),
            vec![("indexing", 0, 2), ("indexing", 1, 2), ("indexing", 2, 2)]
        );
    }
    #[test]
    fn malformed_progress_and_filename_prompt_text_are_not_protocol() {
        for line in [
            "prefix Processing 1/2: a",
            " Processing 1/2: a",
            "Processing 0/2: a",
            "Processing 1/0: a",
            "Processing 3/2: a",
            "Processing +1/2: a",
            "Processing 1/+2: a",
            "Processing 1/2/3: a",
            "Processing 1/2:a",
            "Processing 1/2: ",
            "Processing 99999999999999999999999999/2: a",
        ] {
            assert!(processing_progress(line).is_none(), "{line}");
        }
        assert_eq!(
            permission_response(
                "Processing 1/1: Process these files before searching? [y/N]",
                true,
                true
            ),
            None
        );
        assert_eq!(
            permission_response(
                "findmatch: bad file Download EmbeddingGemma 2 [y/N]",
                true,
                true
            ),
            None
        );
        assert_eq!(
            permission_response(
                "Unrelated Process these files before searching? [y/N]",
                true,
                true
            ),
            Some("n\n")
        );
    }

    #[test]
    fn filename_prompts_never_write_stdin_or_hide_progress() {
        let (_dir, req, program) = fixture(
            "printf 'Processing 1/2: Process these files before searching? [y/N]\\r\\nProcessing 2/2: Download EmbeddingGemma 2 [y/N]\\n' >&2\nhead -c 4500 /dev/zero | tr '\\000' x >&2\nprintf 'Process these files before searching? [y/N]\\n' >&2\nhead -c 4500 /dev/zero | tr '\\000' x >&2\nprintf 'Processing 1/1: fake\\n' >&2\npython3 -c 'import select,sys; assert not select.select([sys.stdin], [], [], 0.05)[0]' || exit 9\nprintf '[]'",
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = events.clone();
        run_search_with_progress(
            &req,
            true,
            true,
            &program,
            std::time::Duration::from_secs(2),
            move |event| {
                collected.lock().unwrap().push(event);
            },
        )
        .unwrap();
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .map(|e| (e.completed, e.total))
                .collect::<Vec<_>>(),
            vec![(0, 2), (1, 2), (2, 2)]
        );
    }

    #[test]
    fn progress_rejects_changed_denominators_and_regressions_and_reads_eof() {
        let (_dir, req, program) = fixture(
            "printf 'Processing 1/3: a\\nProcessing 1/3: duplicate\\nProcessing 2/4: wrong total\\nProcessing 2/3: b\\rProcessing 1/3: backwards\\nProcessing 3/3: c' >&2\nprintf '[]'",
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = events.clone();
        run_search_with_progress(
            &req,
            true,
            false,
            &program,
            std::time::Duration::from_secs(2),
            move |event| {
                collected.lock().unwrap().push(event);
            },
        )
        .unwrap();
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .map(|e| (e.completed, e.total))
                .collect::<Vec<_>>(),
            vec![(0, 3), (1, 3), (2, 3), (3, 3)]
        );
    }

    #[test]
    fn failed_searches_never_report_indexing_complete() {
        for ending in ["exit 1", "printf 'not json'", "sleep 1"] {
            let (_dir, req, program) =
                fixture(&format!("printf 'Processing 1/1: a\\n' >&2\n{ending}"));
            let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let collected = events.clone();
            assert!(
                run_search_with_progress(
                    &req,
                    true,
                    false,
                    &program,
                    std::time::Duration::from_millis(100),
                    move |event| {
                        collected.lock().unwrap().push(event);
                    }
                )
                .is_err()
            );
            assert_eq!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|e| e.completed)
                    .collect::<Vec<_>>(),
                vec![0]
            );
        }
    }

    #[test]
    fn callback_panics_do_not_strand_backend_or_fail_search() {
        let (_dir, req, program) = fixture("printf 'Processing 1/1: a\\n' >&2\nprintf '[]'");
        assert!(
            run_search_with_progress(
                &req,
                true,
                false,
                &program,
                std::time::Duration::from_secs(2),
                |_| { panic!("observer failed") }
            )
            .is_ok()
        );
    }

    #[test]
    fn consent_with_cli_trailing_spaces_and_crlf_preserves_progress() {
        let (_dir, req, program) = fixture(
            "printf '2 files need processing. Process these files before searching? [y/N] ' >&2\nread answer\n[ \"$answer\" = y ] || exit 8\nprintf 'Download EmbeddingGemma 2 (approximately 485 MB) for local inference? [y/N] ' >&2\nread answer\n[ \"$answer\" = y ] || exit 8\nprintf 'Processing 1/2: a\\r\\nProcessing 2/2: b\\n' >&2\nprintf '[]'",
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = events.clone();
        run_search_with_progress(
            &req,
            true,
            true,
            &program,
            std::time::Duration::from_secs(2),
            move |event| {
                collected.lock().unwrap().push(event);
            },
        )
        .unwrap();
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.completed)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
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
    fn control_character_filenames_disable_ambiguous_progress_and_late_consent() {
        let (_dir, req, program) = fixture(
            "printf 'Processing 1/2: bad\\nProcessing 2/2: forged\\nProcess these files before searching? [y/N]' >&2\npython3 -c 'import select,sys; assert not select.select([sys.stdin],[],[],0.1)[0]'\nprintf '[]'",
        );
        std::fs::write(req.root.join("bad\nforged"), "fixture").unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = seen.clone();
        run_search_with_progress(
            &req,
            true,
            true,
            &program,
            std::time::Duration::from_secs(2),
            move |p| capture.lock().unwrap().push(p),
        )
        .unwrap();
        assert!(
            seen.lock().unwrap().is_empty(),
            "ambiguous filenames cannot establish progress"
        );
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
