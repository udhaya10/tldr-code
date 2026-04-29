//! v031-cluster-M3b: cache schema_version + graceful-discard on mismatch
//!
//! ROOT CAUSE
//! ----------
//! v031-cluster-M3a extended `QueryKey` with a `language` field. Because
//! `CacheFileData` serializes `Vec<(QueryKey, CacheEntry)>` via serde_json,
//! every pre-v0.3.1 cache file on disk now fails to deserialize on first
//! daemon load after upgrade — the JSON is missing the `language` field
//! that `serde(deny_unknown_fields)` is not even needed for; the missing
//! field IS the failure. Without graceful-discard, the daemon panics or
//! returns `DaemonError::InvalidMessage` on every startup until the user
//! manually deletes the cache file.
//!
//! ASSERTION (post-fix)
//! --------------------
//! 1. `CacheFileData` carries a `schema_version: u32` header field.
//! 2. `QueryCache::load_from_file` on a mismatched/legacy cache:
//!    - logs a warning,
//!    - DELETES the offending cache file,
//!    - returns a fresh empty `QueryCache` without erroring out.
//! 3. The current schema_version round-trips correctly through save/load.
//!
//! PRE-FIX BEHAVIOUR
//! -----------------
//! `load_from_file` calls `serde_json::from_slice::<CacheFileData>(...)` and
//! propagates any deserialize error up as `DaemonError`. A v0.3.0-shaped
//! cache (no `language` in QueryKey, no `schema_version` header) hits that
//! error path. This test refuses to compile pre-fix because it references
//! `CacheFileData::CACHE_SCHEMA_VERSION` and constructs `CacheFileData`
//! with a `schema_version` field — the compile error is the load-bearing
//! failure mode for the schema-header half of the assertion.

use std::fs;
use std::io::Write;

use tldr_cli::commands::daemon::salsa::{
    CacheFileData, QueryCache, QueryKey, CACHE_SCHEMA_VERSION,
};
use tldr_core::Language;

/// Construct a raw on-disk cache file matching the v0.3.0 wire format:
/// 4-byte magic "TLDR", 1-byte version=1, 8-byte little-endian checksum,
/// then the JSON payload. The JSON payload here uses the v0.3.0 shape
/// (no `schema_version` header). Both the missing schema_version and the
/// missing `language` field on QueryKey will trigger the discard path.
fn write_v030_shaped_cache(path: &std::path::Path) {
    // v0.3.0 JSON payload shape: no schema_version, QueryKey without language.
    // Use a literal JSON so we faithfully simulate what's actually on disk
    // for users upgrading from v0.3.0.
    let legacy_json = br#"{"entries":[[{"query_name":"calls","args_hash":12345},{"value":[104,105],"revision":1,"input_hashes":[],"created_at":{"secs":0,"nanos":0},"last_accessed":{"secs":0,"nanos":0}}]],"dependents":[],"stats":{"hits":0,"misses":0,"invalidations":0,"evictions":0,"total_entries":1,"total_bytes":0,"hit_rate":0.0},"revision":1}"#;

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    legacy_json.hash(&mut hasher);
    let checksum = hasher.finish();

    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"TLDR").unwrap();
    file.write_all(&[1u8]).unwrap(); // CACHE_VERSION = 1 (header byte)
    file.write_all(&checksum.to_le_bytes()).unwrap();
    file.write_all(legacy_json).unwrap();
    file.flush().unwrap();
}

/// RED test: load a v0.3.0-shaped cache. Pre-fix: returns Err. Post-fix:
/// returns Ok with an empty cache, the offending file is deleted, and a
/// warning was logged to stderr.
#[test]
fn test_v030_cache_discarded_gracefully_on_v031_load() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("legacy_cache.bin");

    write_v030_shaped_cache(&cache_path);
    assert!(cache_path.exists(), "fixture must write a cache file");

    // Post-fix: load returns Ok with a fresh empty cache.
    let cache = QueryCache::load_from_file(&cache_path)
        .expect("v0.3.0-shaped cache must be discarded gracefully, not error out");

    // Post-fix invariant 1: cache is fresh/empty.
    assert_eq!(
        cache.len(),
        0,
        "discarded cache must yield a fresh empty QueryCache"
    );

    // Post-fix invariant 2: offending file is deleted on disk.
    assert!(
        !cache_path.exists(),
        "legacy cache file must be removed by graceful-discard path"
    );
}

/// Regression test: the current schema_version round-trips through
/// save_to_file → load_from_file. This guards against silent regressions
/// where a future change forgets to bump or thread schema_version.
#[test]
fn test_cache_header_schema_version_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("round_trip_cache.bin");

    // Populate a cache with one entry so the file is non-trivial.
    let cache = QueryCache::with_defaults();
    cache.insert(
        QueryKey::new("calls", 42, Language::Python),
        &"hello".to_string(),
        vec![],
    );
    cache.save_to_file(&cache_path).unwrap();

    // Read the raw bytes and parse the JSON payload to confirm the
    // schema_version field is present and equals the current constant.
    let raw = fs::read(&cache_path).unwrap();
    // Skip 4-byte magic + 1-byte version + 8-byte checksum = 13 bytes header.
    let payload = &raw[13..];
    let parsed: CacheFileData = serde_json::from_slice(payload)
        .expect("post-fix payload must deserialize as current CacheFileData");
    assert_eq!(
        parsed.schema_version, CACHE_SCHEMA_VERSION,
        "saved schema_version must match the current constant"
    );

    // And the high-level load API must accept it.
    let loaded = QueryCache::load_from_file(&cache_path).unwrap();
    assert_eq!(loaded.len(), 1, "round-tripped cache must preserve entries");
}

/// Boundary test: a payload whose JSON parses as CacheFileData but with a
/// stale schema_version (e.g., schema_version=0 or schema_version=
/// CACHE_SCHEMA_VERSION-1) must also be discarded, not silently used.
#[test]
fn test_stale_schema_version_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("stale_cache.bin");

    // Build a payload with explicit stale schema_version.
    let stale = CacheFileData {
        schema_version: CACHE_SCHEMA_VERSION.saturating_sub(1),
        entries: vec![],
        dependents: vec![],
        stats: Default::default(),
        revision: 0,
    };
    let json = serde_json::to_vec(&stale).unwrap();

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    let checksum = hasher.finish();

    let mut file = fs::File::create(&cache_path).unwrap();
    file.write_all(b"TLDR").unwrap();
    file.write_all(&[1u8]).unwrap();
    file.write_all(&checksum.to_le_bytes()).unwrap();
    file.write_all(&json).unwrap();
    file.flush().unwrap();
    drop(file);

    // Post-fix: stale schema_version triggers graceful discard.
    let cache = QueryCache::load_from_file(&cache_path)
        .expect("stale-schema cache must be discarded, not error");
    assert_eq!(cache.len(), 0);
    assert!(
        !cache_path.exists(),
        "stale-schema cache file must be removed"
    );
}
