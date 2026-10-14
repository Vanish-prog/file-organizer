//! Duplicate suggestions, not authorization to delete or trash files.
//!
//! Metadata-keyed persistent cache is only an optimization: deliberately restored
//! mtimes or inode reuse can evade freshness detection. Callers must revalidate
//! user-selected content independently before destructive actions. This engine
//! never mutates collection files. It assumes a quiescent, user-controlled tree.
//! Progress callbacks execute serially on scan workers and should return quickly;
//! counts reset per stage, while content bytes and cache hits are cumulative.

use crate::core::FileEntry;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

fn checked_read<T>(path: &Path, read: impl FnOnce(fs::File) -> Result<T>) -> Result<T> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "duplicate input must be a regular non-symlink file"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    // This application targets Linux; O_NOFOLLOW also closes final-component races.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    ensure!(
        identity(&before) == identity(&opened)
            && before.ctime() == opened.ctime()
            && before.ctime_nsec() == opened.ctime_nsec(),
        "file replaced before read"
    );
    let observer = file.try_clone()?;
    let value = read(file)?;
    let after = observer.metadata()?;
    ensure!(
        identity(&opened) == identity(&after)
            && opened.ctime() == after.ctime()
            && opened.ctime_nsec() == after.ctime_nsec(),
        "file changed while reading"
    );
    stable(path, &before)?;
    Ok(value)
}

fn exact_hash(path: &Path, bytes: &AtomicU64) -> Result<[u8; 32]> {
    checked_read(path, |mut file| {
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes.fetch_add(count as u64, Ordering::Relaxed);
            hash.update(&buffer[..count]);
        }
        Ok(hash.finalize().into())
    })
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Visual {
    hash: [u64; 4],
    gray: Vec<u8>,
    color: [f64; 3],
    aspect: f64,
    deviation: f64,
}

struct Counted<'a> {
    file: fs::File,
    bytes: &'a AtomicU64,
}
impl Read for Counted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buffer)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
impl Seek for Counted<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

fn visual(path: &Path, bytes: &AtomicU64) -> Result<Option<Visual>> {
    checked_read(path, |file| decode_visual(file, bytes))
}
fn decode_visual(file: fs::File, bytes: &AtomicU64) -> Result<Option<Visual>> {
    let mut reader =
        image::ImageReader::new(BufReader::new(Counted { file, bytes })).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(image::ImageError::IoError(error)) => return Err(error.into()),
        Err(_) => return Ok(None),
    };
    if image.width() < 16 || image.height() < 16 {
        return Ok(None);
    }
    let rgb = image
        .resize_exact(16, 16, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let gray_image = image
        .resize_exact(17, 16, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = [0u64; 4];
    for y in 0..16 {
        for x in 0..16 {
            let i = (y * 16 + x) as usize;
            if gray_image.get_pixel(x, y)[0] > gray_image.get_pixel(x + 1, y)[0] {
                hash[i / 64] |= 1 << (i % 64);
            }
        }
    }
    let gray = image
        .resize_exact(16, 16, image::imageops::FilterType::Triangle)
        .to_luma8()
        .into_raw();
    let mean = gray.iter().map(|&v| f64::from(v)).sum::<f64>() / 256.0;
    let deviation = (gray
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / 256.0)
        .sqrt();
    let mut color = [0.0; 3];
    for pixel in rgb.pixels() {
        for c in 0..3 {
            color[c] += f64::from(pixel[c]) / 256.0;
        }
    }
    Ok(Some(Visual {
        hash,
        gray,
        color,
        aspect: f64::from(image.width()) / f64::from(image.height()),
        deviation,
    }))
}

fn visual_distance(a: &Visual, b: &Visual) -> Option<u32> {
    if a.deviation < 10.0 || b.deviation < 10.0 || ((a.aspect / b.aspect) - 1.0).abs() > 0.03 {
        return None;
    }
    let distance = a
        .hash
        .iter()
        .zip(&b.hash)
        .map(|(a, b)| (a ^ b).count_ones())
        .sum::<u32>();
    if distance > 8 {
        return None;
    }
    let difference = a
        .gray
        .iter()
        .zip(&b.gray)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
        .sum::<f64>()
        / 256.0;
    if difference > 8.0
        || a.color
            .iter()
            .zip(&b.color)
            .any(|(a, b)| (a - b).abs() > 12.0)
    {
        return None;
    }
    Some(distance)
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub stage: String,
    pub completed: usize,
    pub total: usize,
    pub bytes_read: u64,
    pub cache_hits: usize,
}

pub fn find_duplicates(reference: &Path, entries: &[FileEntry]) -> Result<Vec<Duplicate>> {
    find_duplicates_with_progress(reference, entries, |_| {})
}

pub fn find_duplicates_with_progress(
    reference: &Path,
    entries: &[FileEntry],
    progress: impl Fn(ScanProgress) + Send + Sync,
) -> Result<Vec<Duplicate>> {
    let mut combined = entries.to_vec();
    combined.push(FileEntry {
        path: reference.into(),
        name: String::new(),
        size: 0,
        dimensions: None,
    });
    let (fingerprints, bytes) = fingerprints(&combined, &progress)?;
    let reference_path = fs::canonicalize(reference)?;
    let reference_index = fingerprints
        .iter()
        .position(|f| fs::canonicalize(&f.0).ok().as_ref() == Some(&reference_path))
        .unwrap();
    let reference_visual = &fingerprints[reference_index].2;
    let reference_hash = fingerprints[reference_index].1;
    let reference_path = fs::canonicalize(reference)?;
    let mut matches = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (entry_path, hash, entry_visual) in &fingerprints {
        let path = fs::canonicalize(entry_path)?;
        if path == reference_path || !seen.insert(path) {
            continue;
        }
        if hash.is_some() && *hash == reference_hash {
            matches.push(Duplicate {
                path: entry_path.clone(),
                kind: "exact".into(),
                distance: 0,
            });
        } else if let (Some(a), Some(b)) = (reference_visual, entry_visual)
            && let Some(distance) = visual_distance(a, b)
        {
            matches.push(Duplicate {
                path: entry_path.clone(),
                kind: "perceptual".into(),
                distance,
            });
        }
    }
    report(
        &progress,
        "complete",
        fingerprints.len(),
        fingerprints.len(),
        &bytes,
    );
    matches.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(matches)
}

/// Deterministic disjoint complete-link groups: each member matches every other.
/// Avoids connected-component chaining at the cost of conservative undergrouping.
pub fn groups(entries: &[FileEntry]) -> Result<Vec<Vec<PathBuf>>> {
    groups_with_progress(entries, |_| {})
}

pub fn groups_with_progress(
    entries: &[FileEntry],
    progress: impl Fn(ScanProgress) + Send + Sync,
) -> Result<Vec<Vec<PathBuf>>> {
    let (fingerprints, bytes) = fingerprints(entries, &progress)?;
    let matches = |a: usize, b: usize| {
        let a = &fingerprints[a];
        let b = &fingerprints[b];
        a.1.is_some() && a.1 == b.1
            || match (&a.2, &b.2) {
                (Some(a), Some(b)) => visual_distance(a, b).is_some(),
                _ => false,
            }
    };
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    // Only cluster representatives need indexing: complete-link membership
    // necessarily matches the representative. Sorted IDs retain first-fit order.
    let mut exact = HashMap::<[u8; 32], Vec<usize>>::new();
    let mut visual = HashMap::<(usize, u32), Vec<usize>>::new();
    for index in 0..fingerprints.len() {
        let mut candidates = BTreeSet::<usize>::new();
        if let Some(hash) = fingerprints[index].1
            && let Some(ids) = exact.get(&hash)
        {
            candidates.extend(ids);
        }
        if let Some(v) = &fingerprints[index].2 {
            for (block, key) in blocks(&v.hash).into_iter().enumerate() {
                if let Some(ids) = visual.get(&(block, key)) {
                    candidates.extend(ids);
                }
            }
        }
        if let Some(id) = candidates
            .into_iter()
            .find(|&id| clusters[id].iter().all(|&member| matches(index, member)))
        {
            clusters[id].push(index);
        } else {
            let id = clusters.len();
            clusters.push(vec![index]);
            if let Some(hash) = fingerprints[index].1 {
                exact.entry(hash).or_default().push(id);
            }
            if let Some(v) = &fingerprints[index].2 {
                for (block, key) in blocks(&v.hash).into_iter().enumerate() {
                    visual.entry((block, key)).or_default().push(id);
                }
            }
        }
        report(&progress, "grouping", index + 1, fingerprints.len(), &bytes);
    }
    report(
        &progress,
        "complete",
        fingerprints.len(),
        fingerprints.len(),
        &bytes,
    );
    Ok(clusters
        .into_iter()
        .filter(|cluster| cluster.len() > 1)
        .map(|cluster| {
            cluster
                .into_iter()
                .map(|i| fingerprints[i].0.clone())
                .collect()
        })
        .collect())
}

fn report(
    progress: &impl Fn(ScanProgress),
    stage: &str,
    completed: usize,
    total: usize,
    bytes: &Stats,
) {
    // Serialize worker delivery as well as count allocation; callbacks see no regressions.
    let mut state = bytes.progress.lock().unwrap_or_else(|e| e.into_inner());
    if state.0 != stage {
        *state = (stage.into(), 0);
    }
    state.1 = state.1.max(completed);
    progress(ScanProgress {
        stage: stage.into(),
        completed: state.1,
        total,
        bytes_read: bytes.read.load(Ordering::Relaxed),
        cache_hits: bytes.hits.load(Ordering::Relaxed),
    });
}

#[derive(Default)]
struct Stats {
    read: AtomicU64,
    hits: AtomicUsize,
    progress: std::sync::Mutex<(String, usize)>,
}

const CACHE_VERSION: u32 = 1;
const MAX_CACHE_ENTRY: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Identity {
    dev: u64,
    inode: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
}
fn identity(m: &fs::Metadata) -> Identity {
    Identity {
        dev: m.dev(),
        inode: m.ino(),
        size: m.len(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
    }
}
// ctime changes on hard-link/unlink flatten; it guards reads, not reuse.
fn stable(path: &Path, before: &fs::Metadata) -> Result<()> {
    let after = fs::symlink_metadata(path)?;
    ensure!(
        after.is_file()
            && !after.file_type().is_symlink()
            && identity(before) == identity(&after)
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "file changed during duplicate scan"
    );
    Ok(())
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    version: u32,
    key: Identity,
    sample: Option<[u8; 32]>,
    hash: Option<[u8; 32]>,
    visual: Option<Visual>,
}
fn cache_directory(files: &[(PathBuf, u64)]) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))?;
    let dir = outside_collection(&base.join("file-organizer/duplicates-v1"), files)?;
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}
fn outside_collection(directory: &Path, files: &[(PathBuf, u64)]) -> Option<PathBuf> {
    // Resolve the existing ancestor before creating anything: lexical checks
    // alone permit XDG symlink aliases to put cache files inside the collection.
    let mut existing = directory.to_path_buf();
    let mut missing = Vec::new();
    while !existing.exists() {
        missing.push(existing.file_name()?.to_os_string());
        existing.pop();
    }
    let mut resolved = fs::canonicalize(existing).ok()?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    if files.iter().any(|(p, _)| {
        p.parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .is_some_and(|parent| resolved.starts_with(parent))
    }) {
        return None;
    }
    Some(resolved)
}
fn cache_path(dir: &Path, key: &Identity) -> PathBuf {
    dir.join(format!(
        "{}-{}-{}-{}-{}.json",
        key.dev, key.inode, key.size, key.mtime, key.mtime_ns
    ))
}
fn load_cache(dir: &Path, key: &Identity) -> Option<CacheEntry> {
    let path = cache_path(dir, key);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_CACHE_ENTRY {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let value: CacheEntry = serde_json::from_reader(file.take(MAX_CACHE_ENTRY + 1)).ok()?;
    if value.version != CACHE_VERSION || &value.key != key {
        return None;
    }
    if let Some(v) = &value.visual
        && (v.gray.len() != 256
            || !v.aspect.is_finite()
            || v.aspect <= 0.0
            || !v.deviation.is_finite()
            || v.color.iter().any(|v| !v.is_finite()))
    {
        return None;
    }
    Some(value)
}
fn save_cache(dir: &Path, value: &CacheEntry) {
    // Best effort, no cache failure can prevent a scan. Atomic rename avoids partial readers.
    let path = cache_path(dir, &value.key);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let Ok(data) = serde_json::to_vec(value) else {
        return;
    };
    let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
    else {
        return;
    };
    use std::io::Write;
    if file.write_all(&data).is_ok() {
        let _ = fs::rename(&temporary, path);
    }
    let _ = fs::remove_file(temporary);
}

type Fingerprint = (PathBuf, Option<[u8; 32]>, Option<Visual>);

fn sample(path: &Path, size: u64, bytes: &AtomicU64) -> Result<[u8; 32]> {
    checked_read(path, |mut file| {
        let mut hash = Sha256::new();
        let count = size.min(4096) as usize;
        let mut buffer = vec![0; count];
        for offset in [
            0,
            size.saturating_sub(count as u64) / 2,
            size.saturating_sub(count as u64),
        ] {
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buffer)?;
            bytes.fetch_add(count as u64, Ordering::Relaxed);
            hash.update(&buffer);
        }
        Ok(hash.finalize().into())
    })
}

fn fingerprints(
    entries: &[FileEntry],
    progress: &(impl Fn(ScanProgress) + Send + Sync),
) -> Result<(Vec<Fingerprint>, Stats)> {
    let bytes = Stats::default();
    let mut sorted = entries.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let mut sizes = HashMap::<u64, usize>::new();
    for entry in sorted {
        let meta = fs::symlink_metadata(&entry.path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "duplicate input must be a regular non-symlink file"
        );
        if seen.insert(fs::canonicalize(&entry.path)?) {
            *sizes.entry(meta.len()).or_default() += 1;
            files.push((entry.path.clone(), meta.len()));
        }
    }
    let total = files.len();
    let cache = cache_directory(&files);
    let done = AtomicUsize::new(0);
    report(progress, "fingerprints", 0, total, &bytes);
    let partial = parallel_map(&files, |(path, size)| {
        let before = fs::symlink_metadata(path)?;
        let key = identity(&before);
        let cached = cache.as_ref().and_then(|dir| load_cache(dir, &key));
        let mut dirty = cached.is_none();
        let mut cacheable = true;
        let mut value = if let Some(value) = cached {
            bytes.hits.fetch_add(1, Ordering::Relaxed);
            value
        } else {
            let visual = match visual(path, &bytes.read) {
                Ok(value) => value,
                Err(_) => {
                    // Permission/read failures can recover without changing the key.
                    // Do not persist a negative visual result from a transient read.
                    cacheable = false;
                    None
                }
            };
            CacheEntry {
                version: CACHE_VERSION,
                key: key.clone(),
                sample: None,
                hash: None,
                visual,
            }
        };
        if sizes[size] > 1 && value.sample.is_none() {
            value.sample = Some(sample(path, *size, &bytes.read)?);
            dirty = true;
        }
        stable(path, &before)?;
        report(
            progress,
            "fingerprints",
            done.fetch_add(1, Ordering::Relaxed) + 1,
            total,
            &bytes,
        );
        Ok((path.clone(), *size, before, value, cacheable, dirty))
    })?;
    let mut samples = HashMap::<(u64, [u8; 32]), usize>::new();
    for (_, size, _, value, _, _) in &partial {
        if let Some(digest) = value.sample {
            *samples.entry((*size, digest)).or_default() += 1;
        }
    }
    done.store(0, Ordering::Relaxed);
    report(progress, "hashes", 0, total, &bytes);
    let result = parallel_map(&partial, |(path, size, before, value, cacheable, dirty)| {
        let mut value = value.clone();
        let mut dirty = *dirty;
        let potential = value.sample.is_some_and(|s| samples[&(*size, s)] > 1);
        if potential && value.hash.is_none() {
            value.hash = Some(exact_hash(path, &bytes.read)?);
            // A cached entry can acquire an exact hash when new peers appear.
            dirty = true;
        }
        stable(path, before)?;
        if *cacheable
            && dirty
            && let Some(dir) = &cache
        {
            save_cache(dir, &value);
        }
        report(
            progress,
            "hashes",
            done.fetch_add(1, Ordering::Relaxed) + 1,
            total,
            &bytes,
        );
        Ok((
            path.clone(),
            if potential { value.hash } else { None },
            value.visual,
        ))
    })?;
    Ok((result, bytes))
}

fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 4)
}
fn parallel_map<T: Sync, U: Send>(
    input: &[T],
    work: impl Fn(&T) -> Result<U> + Sync,
) -> Result<Vec<U>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let chunk = input.len().div_ceil(worker_count());
    std::thread::scope(|scope| {
        let handles = input
            .chunks(chunk)
            .map(|part| {
                let work = &work;
                scope.spawn(move || part.iter().map(work).collect::<Result<Vec<_>>>())
            })
            .collect::<Vec<_>>();
        let mut output = Vec::with_capacity(input.len());
        for handle in handles {
            let part = handle
                .join()
                .map_err(|_| anyhow::anyhow!("duplicate worker panicked"))??;
            output.extend(part);
        }
        Ok(output)
    })
}

// 256 bits partitioned into nine nonoverlapping blocks (28 or 29 bits).
// At most eight changed bits leave at least one block unchanged.
fn blocks(hash: &[u64; 4]) -> [u32; 9] {
    let mut result = [0; 9];
    for (block, value) in result.iter_mut().enumerate() {
        let start = block * 256 / 9;
        let end = (block + 1) * 256 / 9;
        for bit in start..end {
            *value |= (((hash[bit / 64] >> (bit % 64)) & 1) as u32) << (bit - start);
        }
    }
    result
}

#[cfg(test)]
mod index_tests {
    use super::*;
    #[test]
    fn visual_decode_preserves_io_errors() {
        // Opening a directory succeeds on Unix, but reading it fails with EISDIR.
        let directory = tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap();
        let bytes = AtomicU64::new(0);
        let error = decode_visual(fs::File::open(directory.path()).unwrap(), &bytes)
            .err()
            .expect("read failures must remain transient errors");
        assert!(error.downcast_ref::<std::io::Error>().is_some());
    }
    #[test]
    fn malformed_oversized_and_wrong_version_cache_are_misses() {
        let d = tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap();
        let source = d.path().join("source");
        fs::write(&source, b"data").unwrap();
        let key = identity(&fs::metadata(source).unwrap());
        let path = cache_path(d.path(), &key);
        for data in [
            b"not json".to_vec(),
            vec![b' '; MAX_CACHE_ENTRY as usize + 1],
        ] {
            fs::write(&path, data).unwrap();
            assert!(load_cache(d.path(), &key).is_none());
        }
        let mut entry = CacheEntry {
            version: CACHE_VERSION + 1,
            key: key.clone(),
            sample: None,
            hash: None,
            visual: None,
        };
        fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        assert!(load_cache(d.path(), &key).is_none());
        entry.version = CACHE_VERSION;
        entry.visual = Some(Visual {
            hash: [0; 4],
            gray: vec![],
            color: [0.0; 3],
            aspect: 1.0,
            deviation: 10.0,
        });
        fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        assert!(load_cache(d.path(), &key).is_none());
    }
    #[test]
    fn cache_resolves_symlink_location_before_excluding_collection() {
        let d = tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap();
        let inside = d.path().join("collection");
        fs::create_dir(&inside).unwrap();
        let alias = d.path().join("alias");
        std::os::unix::fs::symlink(&inside, &alias).unwrap();
        assert!(outside_collection(&alias.join("cache"), &[(inside.join("a.bin"), 1)]).is_none());
    }
    #[test]
    fn read_guard_rejects_changed_open_file() {
        let d = tempfile::tempdir_in(std::env::var("TMPDIR").unwrap()).unwrap();
        let path = d.path().join("data");
        fs::write(&path, b"before").unwrap();
        let result = checked_read(&path, |_| {
            fs::write(&path, b"changed").unwrap();
            Ok(())
        });
        assert!(result.is_err());
    }
    #[test]
    fn concurrent_progress_cannot_regress_within_a_stage() {
        let stats = Stats::default();
        let seen = std::sync::Mutex::new(Vec::new());
        let callback = |p: ScanProgress| seen.lock().unwrap().push(p.completed);
        report(&callback, "hashes", 2, 3, &stats);
        report(&callback, "hashes", 1, 3, &stats);
        assert_eq!(*seen.lock().unwrap(), vec![2, 2]);
    }
    #[test]
    fn fingerprint_workers_are_conservative_and_ordered() {
        assert!((1..=4).contains(&worker_count()));
        let input = (0..100).collect::<Vec<_>>();
        let output = parallel_map(&input, |n| Ok(n * 2)).unwrap();
        assert_eq!(output, input.into_iter().map(|n| n * 2).collect::<Vec<_>>());
    }
    #[test]
    fn nine_blocks_retain_every_radius_eight_neighbor() {
        let base = [0xabcdef0123456789; 4];
        let mut changed = base;
        for bit in [0, 30, 59, 88, 117, 146, 175, 204] {
            changed[bit / 64] ^= 1 << (bit % 64);
        }
        let a = blocks(&base);
        let b = blocks(&changed);
        assert!(a.iter().zip(b).any(|(a, b)| *a == b));
        assert_eq!(a.len(), 9);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    pub path: PathBuf,
    pub kind: String,
    pub distance: u32,
}
