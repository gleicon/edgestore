use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = PathBuf::from("./edgestore_demo.db");
    println!("opening db at {:?}", db_path);

    let config = EdgestoreConfig::new(&db_path);
    let mut engine = Engine::open(config)?;

    // Snapshot BEFORE writes — captures what is in flushed segments from previous runs.
    // WAL-only data (current session) is not visible here; that is intentional.
    let prev_snap = engine.snapshot()?;
    let prev_count: u64 = prev_snap
        .get(b"meta", b"run_count")?
        .as_deref()
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    drop(prev_snap);

    // Current count comes from the live read path (memtable + segments)
    let run_count: u64 = engine
        .get(b"meta", b"run_count")?
        .as_deref()
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
        + 1;

    println!(
        "run #{} (segments saw #{} before this write)",
        run_count, prev_count
    );

    // Basic put/get
    engine.put(b"meta", b"run_count", run_count.to_string().as_bytes())?;
    engine.put(
        b"runs",
        format!("run-{:04}", run_count).as_bytes(),
        format!("completed at run {}", run_count).as_bytes(),
    )?;

    // TTL write — lazy expiry: readable until compaction removes the cohort
    engine.put_with_ttl(b"ephemeral", b"session", b"active", 60)?;
    let ttl_val = engine.get(b"ephemeral", b"session")?;
    println!(
        "ttl key (60s) visible before compaction: {:?}",
        ttl_val.as_deref().and_then(|v| std::str::from_utf8(v).ok())
    );

    // Transactional batch — atomic: both writes land together or neither does
    let mut tx = engine.begin();
    tx.put(
        b"events",
        format!("evt-{:04}-start", run_count).as_bytes(),
        b"started",
        0,
        0,
    )?;
    tx.put(
        b"events",
        format!("evt-{:04}-end", run_count).as_bytes(),
        b"done",
        0,
        0,
    )?;
    engine.commit_transaction(tx)?;

    // Prefix scan: all run records
    println!("\n--- run history ---");
    for (k, v) in engine.prefix(b"runs", b"")? {
        println!(
            "  {} => {}",
            String::from_utf8_lossy(&k),
            String::from_utf8_lossy(&v)
        );
    }

    // Range scan: events for this run only
    let evt_start = format!("evt-{:04}-", run_count);
    let evt_end = format!("evt-{:04}~", run_count);
    println!("\n--- events this run ---");
    for (k, v) in engine.range(b"events", evt_start.as_bytes(), evt_end.as_bytes())? {
        println!(
            "  {} => {}",
            String::from_utf8_lossy(&k),
            String::from_utf8_lossy(&v)
        );
    }

    // Flush WAL to immutable segments; compact when enough runs accumulate
    if run_count % 9 == 0 {
        print!("\ncompacting... ");
        engine.flush_to_segments()?;
        let stats = engine.compact_once()?;
        println!(
            "done ({} cohorts, -{} segments, +{} segments, {} bytes written)",
            stats.cohorts_collected,
            stats.segments_removed,
            stats.segments_written,
            stats.bytes_written
        );
    } else if run_count % 3 == 0 {
        print!("\nflushing memtable to segments... ");
        engine.flush_to_segments()?;
        println!("done");
    } else {
        engine.flush()?;
    }

    // Engine metrics for this session
    println!("\n--- session metrics ---");
    print!("{}", engine.metrics());

    println!("\n\ndone. run again to see persistent state accumulate.");
    Ok(())
}
