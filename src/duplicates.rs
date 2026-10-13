use crate::core::FileEntry;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

fn exact_hash(path: &Path) -> Result<[u8; 32]> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "duplicate input must be a regular non-symlink file"
    );
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().into())
}

struct Visual {
    hash: [u64; 4],
    gray: Vec<u8>,
    color: [f64; 3],
    aspect: f64,
    deviation: f64,
}

fn visual(path: &Path) -> Option<Visual> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    if image.width() < 16 || image.height() < 16 {
        return None;
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
    Some(Visual {
        hash,
        gray,
        color,
        aspect: f64::from(image.width()) / f64::from(image.height()),
        deviation,
    })
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

/// Exact SHA-256 matches or conservative visual copies, never semantic similarity.
/// Corrupt, oversized and unsupported images retain exact-hash matching only.
pub fn find_duplicates(reference: &Path, entries: &[FileEntry]) -> Result<Vec<Duplicate>> {
    let reference_visual = visual(reference);
    let reference_hash = exact_hash(reference)?;
    let reference_path = fs::canonicalize(reference)?;
    let mut matches = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let hash = exact_hash(&entry.path)?;
        let path = fs::canonicalize(&entry.path)?;
        if path == reference_path || !seen.insert(path) {
            continue;
        }
        if hash == reference_hash {
            matches.push(Duplicate {
                path: entry.path.clone(),
                kind: "exact".into(),
                distance: 0,
            });
        } else if let (Some(a), Some(b)) = (&reference_visual, visual(&entry.path))
            && let Some(distance) = visual_distance(a, &b)
        {
            matches.push(Duplicate {
                path: entry.path.clone(),
                kind: "perceptual".into(),
                distance,
            });
        }
    }
    matches.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(matches)
}

/// Deterministic disjoint complete-link groups: each member matches every other.
/// Avoids connected-component chaining at the cost of conservative undergrouping.
pub fn groups(entries: &[FileEntry]) -> Result<Vec<Vec<PathBuf>>> {
    let mut fingerprints = Vec::new();
    let mut sorted = entries.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen = std::collections::BTreeSet::new();
    for entry in sorted {
        let hash = exact_hash(&entry.path)?;
        if seen.insert(fs::canonicalize(&entry.path)?) {
            fingerprints.push((entry.path.clone(), hash, visual(&entry.path)));
        }
    }
    let matches = |a: usize, b: usize| {
        let a = &fingerprints[a];
        let b = &fingerprints[b];
        a.1 == b.1
            || match (&a.2, &b.2) {
                (Some(a), Some(b)) => visual_distance(a, b).is_some(),
                _ => false,
            }
    };
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for index in 0..fingerprints.len() {
        if let Some(cluster) = clusters
            .iter_mut()
            .find(|cluster| cluster.iter().all(|&member| matches(index, member)))
        {
            cluster.push(index);
        } else {
            clusters.push(vec![index]);
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    pub path: PathBuf,
    pub kind: String,
    pub distance: u32,
}
