//! Filesystem operations for a user-controlled, quiescent directory tree.
//!
//! Sources are regular files; traversal, symlink sources/ancestors and symlink
//! targets are rejected. Destinations are created with no-overwrite hard links,
//! so moves require same-filesystem hard-link support. No permanent deletion is
//! used for merge. Preflight rejects invalid batches before mutation, but I/O
//! failures can produce partial progress, and operations are not crash-atomic.
//! Path-based rechecks reduce accidental changes but do not protect against a
//! hostile process concurrently replacing directories/files (TOCTOU); callers
//! must prevent concurrent tree mutation. Root ancestor symlinks are resolved
//! during canonicalization; the root itself cannot be a symlink.

use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn checked_root(root: &Path) -> Result<PathBuf> {
    // Rebuild components to strip trailing separators before lstat: on Unix,
    // lstat("symlink/") otherwise follows the final directory symlink.
    let root = root.components().collect::<PathBuf>();
    let metadata = fs::symlink_metadata(&root).context("read root metadata")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "root must be a real directory"
    );
    Ok(fs::canonicalize(root)?)
}

/// Lists only immediate regular files; never follows symlinks.
pub fn scan(root: &Path) -> Result<Vec<FileEntry>> {
    let root = checked_root(root)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        entries.push(FileEntry {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            size: metadata.len(),
            dimensions: image::image_dimensions(entry.path()).ok(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    Ok(entries)
}

fn validate_file(root: &Path, path: &Path, direct: bool) -> Result<PathBuf> {
    use std::path::Component;
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    ensure!(
        !path.components().any(|c| matches!(c, Component::ParentDir)),
        "parent traversal is forbidden"
    );
    let relative = path.strip_prefix(root).context("source escapes root")?;
    ensure!(!relative.as_os_str().is_empty(), "source is root");
    if direct {
        ensure!(
            relative.components().count() == 1,
            "source must be a direct root file"
        );
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "invalid source component"
        );
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "symlink source or ancestor is forbidden"
        );
    }
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "source must be a regular file"
    );
    Ok(path)
}

fn available_destination(directory: &Path, source: &Path) -> Result<PathBuf> {
    let name = source.file_name().context("missing source filename")?;
    let mut destination = directory.join(name);
    let stem = source.file_stem().unwrap_or(name);
    let mut suffix = 0u64;
    loop {
        match fs::symlink_metadata(&destination) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(destination),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        suffix = suffix.checked_add(1).context("collision suffix overflow")?;
        let mut name = stem.to_os_string();
        name.push(format!(" ({suffix})"));
        if let Some(extension) = source.extension() {
            name.push(".");
            name.push(extension);
        }
        destination = directory.join(name);
    }
}

// Hard-link creation is atomic and refuses every existing destination, unlike rename.
// A failed unlink leaves both copies; errors never silently discard the source.
fn move_no_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::hard_link(source, destination)
        .context("create no-overwrite destination (same filesystem required)")?;
    fs::remove_file(source).context("remove original after destination creation")?;
    Ok(())
}

/// Moves descendant regular files into root. Leaves symlinks and nonempty directories.
/// Preflight is not a transaction: later I/O errors may leave earlier moves completed.
pub fn flatten(root: &Path) -> Result<usize> {
    let root = checked_root(root)?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .min_depth(1)
    {
        let entry = entry?;
        if entry.file_type().is_dir() {
            directories.push(entry.path().to_path_buf());
        }
        if entry.file_type().is_file() && entry.path().parent() != Some(root.as_path()) {
            files.push(validate_file(&root, entry.path(), false)?);
        }
    }
    files.sort();
    for source in &files {
        validate_file(&root, source, false)?;
        let destination = available_destination(&root, source)?;
        move_no_replace(source, &destination)?;
    }
    directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for directory in directories {
        let metadata = fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "directory changed during flatten"
        );
        if fs::read_dir(&directory)?.next().is_none() {
            fs::remove_dir(directory)?;
        }
    }
    Ok(files.len())
}

/// Moves direct root files into a single child folder after validating every input.
/// Invalid inputs cause no changes. Runtime I/O errors are not transactionally rolled back.
pub fn include(root: &Path, folder: &str, paths: &[PathBuf]) -> Result<usize> {
    use std::{collections::BTreeSet, path::Component};
    let root = checked_root(root)?;
    let mut components = Path::new(folder).components();
    ensure!(
        !folder.contains(['/', '\\'])
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "folder must be a single normal component"
    );
    let target = root.join(folder);
    match fs::symlink_metadata(&target) {
        Ok(m) => ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "target must be a real directory"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut sources = BTreeSet::new();
    for path in paths {
        ensure!(
            sources.insert(validate_file(&root, path, true)?),
            "duplicate source path"
        );
    }
    if sources.is_empty() {
        return Ok(0);
    }
    if !target.exists() {
        fs::create_dir(&target)?;
    }
    for source in &sources {
        let metadata = fs::symlink_metadata(&target)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "target changed during include"
        );
        validate_file(&root, source, true)?;
        let destination = available_destination(&target, source)?;
        move_no_replace(source, &destination)?;
    }
    Ok(sources.len())
}

/// Sends validated direct root files to the OS trash, never the master.
/// The caller chooses matches; this function does not assert duplicate content.
/// Trash failures can leave a partially completed batch; permanent deletion is never used.
pub fn merge_to_trash(root: &Path, master: &Path, others: &[PathBuf]) -> Result<usize> {
    let root = checked_root(root)?;
    let master = validate_file(&root, master, true)?;
    let mut sources = std::collections::BTreeSet::new();
    for path in others {
        let source = validate_file(&root, path, true)?;
        ensure!(source != master, "master cannot be trashed");
        ensure!(sources.insert(source), "duplicate trash source");
    }
    for source in &sources {
        validate_file(&root, &master, true)?;
        validate_file(&root, source, true)?;
        trash::delete(source).context("send file to operating-system trash")?;
    }
    Ok(sources.len())
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub dimensions: Option<(u32, u32)>,
}
