//! L1 in-process cache for MCP tool results
//!
//! Provides a TTL-based, bounded cache that sits in front of tool execution.
//! The cache is keyed on `tool_name:args_json` and stores `ToolsCallResult` values.
//!
//! Design constraints:
//! - Single-threaded server (blocking stdio loop), so `RefCell` is sufficient
//! - `ToolsCallResult` derives `Clone`, so cached values can be returned by clone
//! - TTL-based expiration prevents stale results for filesystem-dependent tools
//! - Max entries bound prevents unbounded memory growth

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::protocol::ToolsCallResult;

/// A cached tool result with insertion timestamp for TTL expiration.
struct CacheEntry {
    result: ToolsCallResult,
    inserted_at: Instant,
}

/// L1 in-process cache for MCP tool results.
///
/// Stores tool results keyed by a deterministic string derived from
/// the tool name and its JSON arguments. Entries expire after `ttl`
/// and the cache is bounded to `max_entries` to prevent unbounded growth.
pub struct L1Cache {
    entries: HashMap<String, CacheEntry>,
    ttl: Duration,
    max_entries: usize,
}

impl L1Cache {
    /// Create a new cache with the given TTL and maximum entry count.
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            max_entries,
        }
    }

    /// Look up a cached result by key.
    ///
    /// Returns `None` if the key is not present or the entry has expired.
    /// Expired entries are not removed here — eviction happens on insert.
    pub fn get(&self, key: &str) -> Option<&ToolsCallResult> {
        self.entries.get(key).and_then(|entry| {
            if entry.inserted_at.elapsed() < self.ttl {
                Some(&entry.result)
            } else {
                None
            }
        })
    }

    /// Insert a tool result into the cache.
    ///
    /// If the cache is at capacity, the oldest entry (by insertion time)
    /// is evicted before inserting the new one.
    pub fn insert(&mut self, key: String, result: ToolsCallResult) {
        if self.entries.len() >= self.max_entries {
            // Evict the oldest entry (earliest inserted_at)
            if let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest_key);
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                result,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Return the number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove a specific key from the cache.
    pub fn invalidate(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Remove all entries from the cache.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Build a deterministic cache key from a tool name and its JSON arguments.
    ///
    /// The key format is `tool_name:sorted_args_json` where object keys are
    /// recursively sorted to ensure `{"a":1,"b":2}` and `{"b":2,"a":1}` produce
    /// the same cache key.
    pub fn cache_key(tool_name: &str, args: &serde_json::Value) -> String {
        let sorted = Self::sort_json_keys(args);
        format!("{}:{}", tool_name, sorted)
    }

    /// Recursively sort all object keys in a JSON value.
    ///
    /// Arrays preserve element order; only object keys are sorted.
    fn sort_json_keys(value: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Object(map) => {
                let mut sorted: serde_json::Map<String, Value> = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), Self::sort_json_keys(&map[key]));
                }
                Value::Object(sorted)
            }
            Value::Array(arr) => Value::Array(arr.iter().map(Self::sort_json_keys).collect()),
            other => other.clone(),
        }
    }
}
