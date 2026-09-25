use clap::Parser;
use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

#[derive(Parser)]
pub struct Create {
    /// Path to create the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Default namespace for the database
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
}

#[derive(Parser)]
pub struct Stats {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Parser)]
pub struct Put {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Key to store
    #[arg(short, long)]
    pub key: String,
    /// Value to store
    #[arg(short, long)]
    pub value: String,
    /// TTL in seconds (optional)
    #[arg(long)]
    pub ttl_seconds: Option<u32>,
    /// Treat value as hex-encoded binary data
    #[arg(long)]
    pub hex: bool,
}

#[derive(Parser)]
pub struct Get {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Key to retrieve
    #[arg(short, long)]
    pub key: String,
    /// Output value as hex-encoded binary
    #[arg(long)]
    pub hex: bool,
}

#[derive(Parser)]
pub struct Delete {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Key to delete
    #[arg(short, long)]
    pub key: String,
}

#[derive(Parser)]
pub struct Range {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the keys
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Start key (inclusive)
    #[arg(short, long)]
    pub start: String,
    /// End key (exclusive)
    #[arg(short, long)]
    pub end: String,
    /// Maximum number of results
    #[arg(short, long)]
    pub limit: Option<usize>,
    /// Output values as hex-encoded binary
    #[arg(long)]
    pub hex: bool,
}

#[derive(Parser)]
pub struct Compact {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Maximum bytes to write during compaction
    #[arg(long)]
    pub write_budget_bytes: Option<u64>,
}

#[derive(serde::Serialize)]
pub struct DatabaseStats {
    pub path: String,
    pub segment_count: usize,
    pub wal_file_count: usize,
    pub total_size_bytes: u64,
    pub metrics: MetricStats,
}

#[derive(serde::Serialize)]
pub struct MetricStats {
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub ranges: u64,
    pub compactions: u64,
    pub segment_flushes: u64,
    pub wal_rotations: u64,
}

pub fn handle_create(cmd: Create) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&cmd.path)
        .map_err(|e| format!("Failed to create database directory: {}", e))?;
    let config = EdgestoreConfig::new(&cmd.path);
    let _engine = Engine::open(config).map_err(|e| format!("Failed to create database: {}", e))?;
    println!("Created database at {}", cmd.path.display());
    Ok(())
}

pub fn handle_stats(cmd: Stats) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let stats = collect_stats(&cmd.path, &engine)?;
    if cmd.json {
        let json = serde_json::to_string_pretty(&stats)?;
        println!("{}", json);
    } else {
        print_stats_table(&stats);
    }
    Ok(())
}

pub fn handle_put(cmd: Put) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let value_bytes = if cmd.hex {
        hex::decode(&cmd.value).map_err(|e| format!("Invalid hex value: {}", e))?
    } else {
        cmd.value.into_bytes()
    };
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    if let Some(ttl) = cmd.ttl_seconds {
        engine
            .put_with_ttl(namespace, key, &value_bytes, ttl)
            .map_err(|e| format!("Failed to store key with TTL: {}", e))?;
        println!("Stored key '{}' with TTL {} seconds", cmd.key, ttl);
    } else {
        engine
            .put(namespace, key, &value_bytes)
            .map_err(|e| format!("Failed to store key: {}", e))?;
        println!("Stored key '{}'", cmd.key);
    }
    Ok(())
}

pub fn handle_get(cmd: Get) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    match engine.get(namespace, key) {
        Ok(Some(value)) => {
            if cmd.hex {
                println!("{}", hex::encode(&value));
            } else {
                match std::str::from_utf8(&value) {
                    Ok(s) => println!("{}", s),
                    Err(_) => println!("(binary) {}", hex::encode(&value)),
                }
            }
            Ok(())
        }
        Ok(None) => {
            eprintln!("Key not found: {}", cmd.key);
            std::process::exit(1);
        }
        Err(e) => Err(format!("Failed to retrieve key: {}", e).into()),
    }
}

pub fn handle_delete(cmd: Delete) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    engine
        .delete(namespace, key)
        .map_err(|e| format!("Failed to delete key: {}", e))?;
    println!("Deleted key '{}'", cmd.key);
    Ok(())
}

pub fn handle_range(cmd: Range) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let namespace = cmd.namespace.as_bytes();
    let start = cmd.start.as_bytes();
    let end = cmd.end.as_bytes();
    let results = engine
        .range(namespace, start, end)
        .map_err(|e| format!("Failed to perform range scan: {}", e))?;
    let limit = cmd.limit.unwrap_or(results.len());
    let count = results.len().min(limit);
    for (key, value) in results.iter().take(limit) {
        let key_str = String::from_utf8_lossy(key);
        if cmd.hex {
            println!("{}={}", hex::encode(key), hex::encode(value));
        } else {
            match std::str::from_utf8(value) {
                Ok(val_str) => println!("{}={}", key_str, val_str),
                Err(_) => println!("{}=(binary) {}", key_str, hex::encode(value)),
            }
        }
    }
    eprintln!("Found {} keys (showing {})", results.len(), count);
    Ok(())
}

pub fn handle_compact(cmd: Compact) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::EdgestoreError;

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let lock_path = cmd.path.join("LOCK");
    if lock_path.exists() {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&lock_path) {
            use fs2::FileExt;
            if file.try_lock_exclusive().is_err() {
                return Err("Database is locked by another process. Please close other connections to this database first.".into());
            }
        }
    }
    let config = if let Some(budget) = cmd.write_budget_bytes {
        let mut c = EdgestoreConfig::new(&cmd.path);
        c.compaction_write_budget_bytes = budget;
        c
    } else {
        EdgestoreConfig::new(&cmd.path)
    };
    let mut engine = match Engine::open(config) {
        Ok(e) => e,
        Err(EdgestoreError::WriterBusy) => {
            return Err("Database is busy (locked by another process). Please close other connections to this database first.".into());
        }
        Err(e) => return Err(format!("Failed to open database: {}", e).into()),
    };
    let initial_segments = count_segment_files(&cmd.path)?;
    let stats = engine
        .compact_once()
        .map_err(|e| format!("Compaction failed: {}", e))?;
    let final_segments = count_segment_files(&cmd.path)?;
    println!("Compaction completed successfully!");
    println!();
    println!("Summary:");
    println!("  Cohorts processed:       {}", stats.cohorts_collected);
    println!("  Segments before:         {}", initial_segments);
    println!("  Segments after:          {}", final_segments);
    println!("  Segments removed:        {}", stats.segments_removed);
    println!("  Segments written:        {}", stats.segments_written);
    println!("  Live records relocated:  {}", stats.live_records_relocated);
    println!("  Bytes relocated:         {}", format_bytes(stats.bytes_written));
    Ok(())
}

fn collect_stats(
    path: &PathBuf,
    engine: &Engine,
) -> Result<DatabaseStats, Box<dyn std::error::Error>> {
    let wal_count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("wal-") && name.ends_with(".log"))
                .unwrap_or(false)
        })
        .count();
    let segment_count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("segment-") && name.ends_with(".dat"))
                .unwrap_or(false)
        })
        .count();
    let m = engine.metrics();
    let total_size = calculate_dir_size(path)?;
    Ok(DatabaseStats {
        path: path.to_string_lossy().to_string(),
        segment_count,
        wal_file_count: wal_count,
        total_size_bytes: total_size,
        metrics: MetricStats {
            puts: m.puts,
            gets: m.gets,
            deletes: m.deletes,
            ranges: m.ranges,
            compactions: m.compactions,
            segment_flushes: m.segment_flushes,
            wal_rotations: m.wal_rotations,
        },
    })
}

fn calculate_dir_size(path: &PathBuf) -> Result<u64, std::io::Error> {
    let mut total_size = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            total_size += metadata.len();
        }
    }
    Ok(total_size)
}

fn count_segment_files(path: &PathBuf) -> Result<usize, std::io::Error> {
    let count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("segment-") && name.ends_with(".dat"))
                .unwrap_or(false)
        })
        .count();
    Ok(count)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

fn print_stats_table(stats: &DatabaseStats) {
    println!("EdgeStore Database Statistics");
    println!("=============================");
    println!();
    println!("Path:              {}", stats.path);
    println!("Segment Files:     {}", stats.segment_count);
    println!("WAL Files:         {}", stats.wal_file_count);
    println!("Total Size:        {} bytes", stats.total_size_bytes);
    println!();
    println!("Operations:");
    println!("  Puts:            {}", stats.metrics.puts);
    println!("  Gets:            {}", stats.metrics.gets);
    println!("  Deletes:         {}", stats.metrics.deletes);
    println!("  Ranges:          {}", stats.metrics.ranges);
    println!();
    println!("Maintenance:");
    println!("  Compactions:     {}", stats.metrics.compactions);
    println!("  Segment Flushes: {}", stats.metrics.segment_flushes);
    println!("  WAL Rotations:   {}", stats.metrics.wal_rotations);
}
