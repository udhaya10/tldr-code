//! v031-cluster-M3a: QueryCache cross-language isolation
//!
//! ROOT CAUSE
//! ----------
//! Pre-fix `QueryKey` is `(query_name: String, args_hash: u64)` — there is
//! no language discriminator. So a Python query result for function `foo`
//! is served back when a TypeScript query for `foo` arrives. This is the
//! cache-contamination half of issue #27 ("`tldr impact` hang/wrong-result
//! class"): once any language has populated a cache key, every subsequent
//! query that hashes to the same `(query_name, args)` tuple inherits that
//! result regardless of the requesting language.
//!
//! ASSERTION (post-fix)
//! --------------------
//! Inserting a result under `(query_name="calls", args, language=Python)`
//! must NOT be observable under `(query_name="calls", args, language=
//! TypeScript)`. The two queries occupy disjoint cache slots.
//!
//! PRE-FIX BEHAVIOUR
//! -----------------
//! `QueryKey::new("calls", hash)` produces a single key shared across
//! languages, so the second `get` returns the Python value (false hit).
//! This file refuses to compile pre-fix because it constructs `QueryKey`
//! with a `language` argument the struct does not yet accept — the
//! compile error is the load-bearing failure mode.

use tldr_cli::commands::daemon::salsa::{hash_args, QueryCache, QueryKey};
use tldr_core::Language;

/// RED test: insert a Python result for `foo`, look the same `foo` up under
/// TypeScript. Pre-fix: returns the Python value (cross-contamination).
/// Post-fix: cache miss for TypeScript (fresh compute path).
#[test]
fn test_query_cache_isolates_by_language() {
    let cache = QueryCache::with_defaults();

    let args_hash = hash_args(&("foo",));
    let py_key = QueryKey::new("calls", args_hash, Language::Python);
    let ts_key = QueryKey::new("calls", args_hash, Language::TypeScript);

    // Sanity: keys with the same (query_name, args_hash) but different
    // languages must compare unequal. This catches the single missing
    // `Hash`/`Eq` derive on the new field.
    assert_ne!(
        py_key, ts_key,
        "QueryKey must distinguish by language: same args + same query_name + \
         Python vs TypeScript MUST be two different keys"
    );

    cache.insert(py_key.clone(), &"python_result", vec![]);

    // Lookup under Python — present.
    let got_py: Option<String> = cache.get(&py_key);
    assert_eq!(
        got_py.as_deref(),
        Some("python_result"),
        "Python lookup of its own insert must return the Python value"
    );

    // Lookup under TypeScript with the SAME args_hash and query_name —
    // pre-fix this returns Some("python_result") because keys collide on
    // (query_name, args_hash). Post-fix this is a miss.
    let got_ts: Option<String> = cache.get(&ts_key);
    assert!(
        got_ts.is_none(),
        "TypeScript lookup must miss when only Python has been inserted; \
         got Some({:?}) — cache is leaking results across languages",
        got_ts
    );
}

/// Discriminative test: prove BOTH languages can independently hold their
/// own cached value for the same (query_name, args_hash) pair. Locks the
/// disjoint-slots property — not just inequality of keys.
#[test]
fn test_query_cache_two_languages_hold_distinct_values_for_same_args() {
    let cache = QueryCache::with_defaults();
    let args_hash = hash_args(&("/proj", "foo"));

    let py_key = QueryKey::new("impact", args_hash, Language::Python);
    let ts_key = QueryKey::new("impact", args_hash, Language::TypeScript);

    cache.insert(py_key.clone(), &"py_callers", vec![]);
    cache.insert(ts_key.clone(), &"ts_callers", vec![]);

    let py_val: Option<String> = cache.get(&py_key);
    let ts_val: Option<String> = cache.get(&ts_key);

    assert_eq!(py_val.as_deref(), Some("py_callers"));
    assert_eq!(ts_val.as_deref(), Some("ts_callers"));
    assert_ne!(
        py_val, ts_val,
        "Two distinct languages must hold distinct values for the same \
         (query_name, args_hash); cross-contamination would collapse them"
    );
}
