use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use edgestore::{EdgestoreConfig, Engine};
use edgestore_text::TextIndex;
use std::collections::HashMap;
use tempfile::TempDir;

fn lcg_sequence(seed: u64, n: usize) -> Vec<u32> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        out.push((s % 1000) as u32);
    }
    out
}

fn generate_text(seed: u64, word_count: usize) -> String {
    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "hello", "world", "rust",
        "code", "data", "search", "index", "token", "text", "engine", "fast", "slow", "red",
        "blue", "green", "yellow", "black", "white", "cat", "dog", "bird", "fish",
    ];
    let indices = lcg_sequence(seed, word_count);
    indices
        .iter()
        .map(|&i| words[i as usize % words.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

fn bench_text_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_search");

    for &n in &[100usize, 1000, 10000] {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        let mut text = TextIndex::new();

        for i in 0..n {
            let doc = generate_text(i as u64 * 12345, 50);
            let key = format!("doc{:08}", i);
            text.index_document(&mut engine, b"ns", key.as_bytes(), &doc, HashMap::new())
                .unwrap();
        }

        group.bench_with_input(BenchmarkId::new("search", n), &n, |b, _| {
            b.iter(|| {
                let results = text
                    .search(&mut engine, b"ns", black_box("quick brown fox"), 10)
                    .unwrap();
                black_box(results);
            });
        });
    }

    group.finish();
}

fn bench_index_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_throughput");

    for &n in &[100usize, 1000, 10000] {
        group.bench_with_input(BenchmarkId::new("index", n), &n, |b, _| {
            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
                    (engine, TextIndex::new(), dir)
                },
                |(mut engine, mut text, _dir)| {
                    for i in 0..n {
                        let doc = generate_text(i as u64 * 12345, 50);
                        let key = format!("doc{:08}", i);
                        text.index_document(
                            &mut engine,
                            b"ns",
                            key.as_bytes(),
                            &doc,
                            HashMap::new(),
                        )
                        .unwrap();
                    }
                    black_box((engine, text));
                },
                criterion::BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

fn bench_delete_and_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_and_search");

    group.bench_function("delete_doc_10k_index", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
                let mut text = TextIndex::new();
                for i in 0..10_000usize {
                    let doc = generate_text(i as u64 * 12345, 50);
                    let key = format!("doc{:08}", i);
                    text.index_document(&mut engine, b"ns", key.as_bytes(), &doc, HashMap::new())
                        .unwrap();
                }
                (engine, text, dir)
            },
            |(mut engine, mut text, _dir)| {
                text.delete_document(&mut engine, b"ns", b"doc00005000")
                    .unwrap();
                let results = text
                    .search(&mut engine, b"ns", black_box("quick brown fox"), 10)
                    .unwrap();
                black_box(results);
            },
            criterion::BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_text_search,
    bench_index_throughput,
    bench_delete_and_search
);
criterion_main!(benches);
