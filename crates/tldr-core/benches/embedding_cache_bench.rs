use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tempfile::tempdir;
use tldr_core::semantic::{CacheConfig, CodeChunk, EmbeddingCache, EmbeddingModel};
use tldr_core::Language;

fn chunk(index: usize) -> CodeChunk {
    CodeChunk {
        file_path: format!("src/file_{index}.rs").into(),
        function_name: Some(format!("function_{index}")),
        class_name: None,
        line_start: 1,
        line_end: 10,
        content: format!("fn function_{index}() {{}}"),
        content_hash: format!("hash-{index}"),
        language: Language::Rust,
        structure: Default::default(),
    }
}

fn vector(index: usize) -> Vec<f32> {
    (0..EmbeddingModel::ArcticS.dimensions())
        .map(|dimension| ((index * 384 + dimension) as f32).sin())
        .collect()
}

fn bench_embedding_cache_operations(c: &mut Criterion) {
    let directory = tempdir().unwrap();
    let config = CacheConfig {
        cache_dir: directory.path().to_path_buf(),
        max_size_mb: 500,
        ttl_days: 30,
    };
    let chunks = (0..10_000).map(chunk).collect::<Vec<_>>();
    let mut cache = EmbeddingCache::open(config.clone()).unwrap();
    for (index, chunk) in chunks.iter().enumerate() {
        cache.put(chunk, vector(index), EmbeddingModel::ArcticS);
    }
    cache.flush().unwrap();

    c.bench_function("embedding_cache_redb_open_10k", |benchmark| {
        benchmark.iter(|| EmbeddingCache::open(black_box(config.clone())).unwrap())
    });

    let query = &chunks[5_000];
    c.bench_function("embedding_cache_redb_hit_384d", |benchmark| {
        benchmark.iter(|| {
            black_box(
                cache
                    .get(black_box(query), EmbeddingModel::ArcticS)
                    .unwrap(),
            )
        })
    });

    let update = &chunks[7_500];
    c.bench_function("embedding_cache_redb_update_one_record", |benchmark| {
        benchmark.iter(|| {
            cache.put(
                black_box(update),
                black_box(vector(7_500)),
                EmbeddingModel::ArcticS,
            );
            cache.flush().unwrap();
        })
    });
}

criterion_group!(benches, bench_embedding_cache_operations);
criterion_main!(benches);
