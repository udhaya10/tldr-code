//! Redb table names and store identity for the shared artifact database.

use redb::{MultimapTableDefinition, TableDefinition};

/// New store filename; legacy cache files are never opened as authoritative.
pub const STORE_FILE: &str = "project.redb";
/// Explicit incompatible store identity.
pub const STORE_SCHEMA: &str = "tldr-artifact-store-v1";
/// Bounded redb page cache.
pub const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("artifact.metadata");
pub(crate) const GENERATIONS: TableDefinition<u64, &[u8]> =
    TableDefinition::new("artifact.generations");
pub(crate) const ARTIFACTS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("artifact.records");
pub(crate) const ARTIFACT_DEPS: MultimapTableDefinition<&[u8], &[u8]> =
    MultimapTableDefinition::new("artifact.dependencies");
pub(crate) const GENERATION_ARTIFACTS: MultimapTableDefinition<u64, &[u8]> =
    MultimapTableDefinition::new("artifact.generation_records");
pub(crate) const JOBS: TableDefinition<&str, &[u8]> = TableDefinition::new("artifact.jobs");

pub(crate) const SCHEMA_KEY: &str = "schema";
pub(crate) const ACTIVE_GENERATION_KEY: &str = "active_generation";
pub(crate) const PREVIOUS_GENERATION_KEY: &str = "previous_generation";
