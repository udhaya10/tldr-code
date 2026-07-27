//! Bounded process-local response memoization.
//!
//! This cache is intentionally non-durable. Reusable analyzer state lives in
//! the redb artifact store; these bytes only avoid repeated serialization
//! within one daemon lifetime.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tldr_core::Language;

use super::types::SalsaCacheStats;

/// Default bound for short-lived rendered responses.
pub const DEFAULT_MAX_ENTRIES: usize = 1_024;

/// Output-independent identity of one hot response.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HotQueryKey {
    query_name: String,
    args_hash: u64,
    language: Language,
}

impl HotQueryKey {
    /// Construct a language-isolated response key.
    pub fn new(query_name: impl Into<String>, args_hash: u64, language: Language) -> Self {
        Self {
            query_name: query_name.into(),
            args_hash,
            language,
        }
    }
}

struct HotEntry {
    value: Vec<u8>,
    inputs: Vec<u64>,
}

/// Bounded in-memory cache for transport-ready values.
pub struct HotResponseCache {
    entries: DashMap<HotQueryKey, HotEntry>,
    dependents: DashMap<u64, HashSet<HotQueryKey>>,
    max_entries: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    invalidations: AtomicU64,
}

impl HotResponseCache {
    /// Construct an explicitly bounded cache.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: DashMap::new(),
            dependents: DashMap::new(),
            max_entries: max_entries.max(1),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
        }
    }

    /// Construct the daemon default.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES)
    }

    /// Decode a process-local transport value.
    pub fn get<T: DeserializeOwned>(&self, key: &HotQueryKey) -> Option<T> {
        let value = self.entries.get(key);
        match value.and_then(|entry| serde_json::from_slice(&entry.value).ok()) {
            Some(value) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(value)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Insert a short-lived rendered value and its source invalidators.
    pub fn insert<T: Serialize>(&self, key: HotQueryKey, value: &T, inputs: Vec<u64>) {
        let Ok(value) = serde_json::to_vec(value) else {
            return;
        };
        if self.entries.len() >= self.max_entries {
            if let Some(oldest) = self.entries.iter().next().map(|entry| entry.key().clone()) {
                self.invalidate(&oldest);
            }
        }
        for input in &inputs {
            self.dependents
                .entry(*input)
                .or_default()
                .insert(key.clone());
        }
        self.entries.insert(key, HotEntry { value, inputs });
    }

    /// Invalidate one rendered response.
    pub fn invalidate(&self, key: &HotQueryKey) -> bool {
        let Some((_, entry)) = self.entries.remove(key) else {
            return false;
        };
        for input in entry.inputs {
            if let Some(mut keys) = self.dependents.get_mut(&input) {
                keys.remove(key);
            }
        }
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Invalidate all rendered responses derived from one source identity.
    pub fn invalidate_by_input(&self, input: u64) -> usize {
        let keys = self
            .dependents
            .remove(&input)
            .map(|(_, keys)| keys)
            .unwrap_or_default();
        keys.into_iter().filter(|key| self.invalidate(key)).count()
    }

    /// Process-local hit/miss counters.
    pub fn stats(&self) -> SalsaCacheStats {
        SalsaCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            recomputations: 0,
        }
    }
}

/// Hash arbitrary typed query arguments.
pub fn hash_args<T: Hash>(args: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    args.hash(&mut hasher);
    hasher.finish()
}

/// Hash a path lexically for same-process invalidation.
pub fn hash_path(path: &Path) -> u64 {
    hash_args(&path.to_string_lossy().replace('\\', "/"))
}

/// Hash exact source bytes.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    hash_args(&bytes)
}
