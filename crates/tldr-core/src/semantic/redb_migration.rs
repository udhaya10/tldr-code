//! One-time reader for the retired whole-map rkyv embedding cache.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::{Mmap, MmapOptions};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::{TldrError, TldrResult};

const CACHE_FILE_PREFIX: &str = "cache.v1.";
const CACHE_FILE_SUFFIX: &str = ".rkyv";

#[derive(Archive, Debug, Clone, PartialEq, RkyvDeserialize, RkyvSerialize)]
struct LegacyCacheEntry {
    embedding: Vec<f32>,
    cached_at: u64,
    file_mtime: Option<u64>,
}

type LegacyDiskMap = HashMap<String, LegacyCacheEntry>;
type ArchivedLegacyDiskMap = rkyv::Archived<LegacyDiskMap>;

struct ValidatedLegacyMmap {
    path: PathBuf,
    mmap: Mmap,
}

impl ValidatedLegacyMmap {
    fn open(path: &Path) -> TldrResult<Self> {
        let file = File::open(path)?;
        if file.metadata()?.len() == 0 {
            return Err(migration_error(path, "empty rkyv archive"));
        }
        // SAFETY: legacy generation files are immutable after publication.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        rkyv::access::<ArchivedLegacyDiskMap, rkyv::rancor::Error>(&mmap[..]).map_err(|error| {
            migration_error(path, format!("archive validation failed: {error}"))
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            mmap,
        })
    }

    fn root(&self) -> &ArchivedLegacyDiskMap {
        // SAFETY: `open` validated these exact immutable bytes.
        unsafe { rkyv::access_unchecked::<ArchivedLegacyDiskMap>(&self.mmap[..]) }
    }
}

/// One migrated legacy cache entry.
#[derive(Debug, Clone, PartialEq)]
pub struct MigratedEmbedding {
    /// Former hexadecimal content-addressed key.
    pub key: String,
    /// Normalized vector.
    pub embedding: Vec<f32>,
    /// Original insertion time as Unix seconds.
    pub cached_at: u64,
    /// Optional source-file modification time.
    pub file_mtime: Option<u64>,
}

/// Result of a one-time migration attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Number of records successfully imported.
    pub imported: usize,
    /// Legacy generation removed after successful import.
    pub removed_generation: Option<PathBuf>,
}

/// Import the newest valid legacy generation, removing it only after every
/// sink call succeeds. Re-running after interruption is idempotent when the
/// sink performs content-addressed upserts.
pub fn migrate_latest(
    cache_dir: &Path,
    mut sink: impl FnMut(MigratedEmbedding) -> TldrResult<()>,
) -> TldrResult<MigrationReport> {
    let generation_paths = legacy_generation_paths(cache_dir);
    if generation_paths.is_empty() {
        return Ok(MigrationReport::default());
    }
    let snapshot = generation_paths
        .iter()
        .find_map(|path| ValidatedLegacyMmap::open(path).ok())
        .ok_or_else(|| migration_error(cache_dir, "no valid legacy rkyv generation"))?;
    let mut imported = 0usize;
    for (key, archived) in snapshot.root().iter() {
        let entry = rkyv::deserialize::<LegacyCacheEntry, rkyv::rancor::Error>(archived).map_err(
            |error| {
                migration_error(
                    &snapshot.path,
                    format!("entry deserialization failed: {error}"),
                )
            },
        )?;
        sink(MigratedEmbedding {
            key: key.to_string(),
            embedding: entry.embedding,
            cached_at: entry.cached_at,
            file_mtime: entry.file_mtime,
        })?;
        imported += 1;
    }
    let migrated_path = snapshot.path.clone();
    drop(snapshot);
    // The newest valid generation is a complete snapshot. Older retained
    // generations must not be imported on the next open after this succeeds.
    for path in generation_paths {
        std::fs::remove_file(path)?;
    }
    // These pre-rkyv formats are removed only after the tested rkyv migration
    // source has been durably imported.
    let _ = std::fs::remove_file(cache_dir.join("cache.json"));
    let _ = std::fs::remove_file(cache_dir.join("cache.bin"));
    Ok(MigrationReport {
        imported,
        removed_generation: Some(migrated_path),
    })
}

fn legacy_generation_paths(cache_dir: &Path) -> Vec<PathBuf> {
    let mut generations = std::fs::read_dir(cache_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let generation = name
                .to_string_lossy()
                .strip_prefix(CACHE_FILE_PREFIX)?
                .strip_suffix(CACHE_FILE_SUFFIX)?
                .parse::<u64>()
                .ok()?;
            Some((generation, entry.path()))
        })
        .collect::<Vec<_>>();
    generations.sort_unstable_by(|left, right| right.0.cmp(&left.0));
    generations.into_iter().map(|(_, path)| path).collect()
}

fn migration_error(path: &Path, message: impl Into<String>) -> TldrError {
    TldrError::ParseError {
        file: path.to_path_buf(),
        line: None,
        message: format!("legacy embedding cache migration: {}", message.into()),
    }
}
