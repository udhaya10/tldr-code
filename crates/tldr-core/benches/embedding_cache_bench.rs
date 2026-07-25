use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;
use tldr_core::semantic::{CacheConfig, CodeChunk, EmbeddingCache, EmbeddingModel};
use tldr_core::Language;

#[derive(Archive, Clone, Deserialize, RkyvDeserialize, RkyvSerialize, Serialize)]
struct BenchEntry {
    embedding: Vec<f32>,
    cached_at: u64,
    file_mtime: Option<u64>,
}

fn dataset() -> HashMap<String, BenchEntry> {
    (0..10_000)
        .map(|i| {
            (
                format!("src/file_{i}.rs:function_{i}:hash:model"),
                BenchEntry {
                    embedding: (0..384)
                        .map(|j| ((i * 384 + j) as f32).sin())
                        .collect(),
                    cached_at: 1_700_000_000 + i,
                    file_mtime: Some(1_700_000_000 + i),
                },
            )
        })
        .collect()
}

fn bench_embedding_cache_formats(c: &mut Criterion) {
    let entries = dataset();
    let json = serde_json::to_vec(&entries).unwrap();
    let archived = rkyv::to_bytes::<rkyv::rancor::Error>(&entries).unwrap();
    eprintln!(
        "embedding cache bytes: JSON={} rkyv={} reduction={:.1}%",
        json.len(),
        archived.len(),
        100.0 * (1.0 - archived.len() as f64 / json.len() as f64)
    );

    let mut serialize = c.benchmark_group("embedding_cache_serialize");
    serialize.bench_function("json", |b| {
        b.iter(|| serde_json::to_vec(black_box(&entries)).unwrap())
    });
    serialize.bench_function("rkyv", |b| {
        b.iter(|| rkyv::to_bytes::<rkyv::rancor::Error>(black_box(&entries)).unwrap())
    });
    serialize.finish();

    let mut open = c.benchmark_group("embedding_cache_open");
    open.bench_function("json_owned_deserialize", |b| {
        b.iter(|| {
            serde_json::from_slice::<HashMap<String, BenchEntry>>(black_box(&json))
                .unwrap()
        })
    });
    open.bench_function("rkyv_validate_zero_copy", |b| {
        b.iter(|| {
            rkyv::access::<
                rkyv::Archived<HashMap<String, BenchEntry>>,
                rkyv::rancor::Error,
            >(black_box(&archived[..]))
            .unwrap()
        })
    });
    open.finish();
}

fn bench_embedding_cache_operations(c: &mut Criterion) {
    let temp = tempdir().unwrap();
    let config = CacheConfig {
        cache_dir: temp.path().to_path_buf(),
        max_size_mb: 500,
        ttl_days: 30,
    };
    let chunks: Vec<_> = (0..10_000)
        .map(|i| CodeChunk {
            file_path: format!("src/file_{i}.rs").into(),
            function_name: Some(format!("function_{i}")),
            class_name: None,
            line_start: 1,
            line_end: 10,
            content: format!("fn function_{i}() {{}}"),
            content_hash: format!("hash-{i}"),
            language: Language::Rust,
        })
        .collect();
    let mut cache = EmbeddingCache::open(config.clone()).unwrap();
    for (i, chunk) in chunks.iter().enumerate() {
        cache.put(
            chunk,
            (0..384)
                .map(|j| ((i * 384 + j) as f32).sin())
                .collect(),
            EmbeddingModel::ArcticS,
        );
    }
    cache.flush().unwrap();

    c.bench_function("embedding_cache_mmap_open_10k", |b| {
        b.iter(|| EmbeddingCache::open(black_box(config.clone())).unwrap())
    });

    let query = &chunks[5_000];
    c.bench_function("embedding_cache_mmap_hit_384d", |b| {
        b.iter(|| {
            black_box(
                cache
                    .get(black_box(query), EmbeddingModel::ArcticS)
                    .unwrap(),
            )
        })
    });
}

criterion_group!(
    benches,
    bench_embedding_cache_formats,
    bench_embedding_cache_operations
);
criterion_main!(benches);
