use criterion::{black_box, criterion_group, criterion_main, Criterion};
use edgestore::segment::SegmentWriter;
use edgestore::types::{encode_key, MemEntry, Operation};
use edgestore::ImmutableEngine;
use tempfile::TempDir;

fn make_segment_bytes(n: usize) -> (edgestore::types::SegmentMeta, Vec<u8>) {
    let dir = TempDir::new().unwrap();
    let ns = b"ns";

    let mut entries: Vec<(Vec<u8>, MemEntry)> = (0..n as u64)
        .map(|i| {
            let enc = encode_key(ns, &i.to_be_bytes());
            let e = MemEntry {
                key: enc.clone(),
                value: Some(b"value".to_vec()),
                op: Operation::Put,
                lsn: i + 1,
                timestamp: 3_600_000_000_000,
                ttl: 0,
            };
            (enc, e)
        })
        .collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    let meta = writer.flush(&entries).unwrap();
    let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
    (meta, dat_bytes)
}

fn bench_immutable_cold_start_1k(c: &mut Criterion) {
    let (meta, bytes) = make_segment_bytes(1000);

    c.bench_function("immutable_cold_start_1k", |b| {
        b.iter(|| {
            let engine =
                ImmutableEngine::from_segment_bytes(vec![(meta.clone(), bytes.clone())]).unwrap();
            let _ = engine.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
        })
    });
}

fn bench_immutable_cold_start_10k(c: &mut Criterion) {
    let (meta, bytes) = make_segment_bytes(10_000);

    c.bench_function("immutable_cold_start_10k", |b| {
        b.iter(|| {
            let engine =
                ImmutableEngine::from_segment_bytes(vec![(meta.clone(), bytes.clone())]).unwrap();
            let _ = engine.get(black_box(b"ns"), black_box(&5000u64.to_be_bytes()));
        })
    });
}

fn bench_immutable_get_hot(c: &mut Criterion) {
    let (meta, bytes) = make_segment_bytes(1000);
    let engine = ImmutableEngine::from_segment_bytes(vec![(meta, bytes)]).unwrap();

    c.bench_function("immutable_get_hot", |b| {
        b.iter(|| {
            let _ = engine.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
        })
    });
}

fn bench_immutable_range_1k(c: &mut Criterion) {
    let (meta, bytes) = make_segment_bytes(1000);
    let engine = ImmutableEngine::from_segment_bytes(vec![(meta, bytes)]).unwrap();

    c.bench_function("immutable_range_1k", |b| {
        b.iter(|| {
            let _ = engine.range(b"ns", &0u64.to_be_bytes(), &1000u64.to_be_bytes());
        })
    });
}

criterion_group!(
    benches,
    bench_immutable_cold_start_1k,
    bench_immutable_cold_start_10k,
    bench_immutable_get_hot,
    bench_immutable_range_1k
);
criterion_main!(benches);
