use clap::{Parser, Subcommand};
use edgestore::{EdgestoreConfig, Engine};
use std::io::Write;
use std::path::PathBuf;

/// EdgeStore CLI — Administrative tool for managing EdgeStore databases
#[derive(Parser)]
#[command(name = "edgestore-cli")]
#[command(about = "EdgeStore database administration tool")]
#[command(version = "1.0.6")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new EdgeStore database
    Create(Create),
    /// Show database statistics
    Stats(Stats),
    /// Store a key-value pair
    Put(Put),
    /// Retrieve a value by key
    Get(Get),
    /// Delete a key
    Delete(Delete),
    /// List keys in a range
    Range(Range),
    /// Run compaction to reclaim space
    Compact(Compact),
    /// Export database to file
    Export(Export),
    /// Import database from file
    Import(Import),
    /// Store a vector
    VectorPut(VectorPut),
    /// Retrieve a vector by key
    VectorGet(VectorGet),
    /// Search for similar vectors
    VectorSearch(VectorSearch),
    /// Search text documents
    TextSearch(TextSearch),
}

#[derive(Parser)]
struct Create {
    /// Path to create the database
    #[arg(short, long)]
    path: PathBuf,
    /// Default namespace for the database
    #[arg(short, long, default_value = "default")]
    namespace: String,
}

#[derive(Parser)]
struct Stats {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct Put {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Key to store
    #[arg(short, long)]
    key: String,
    /// Value to store
    #[arg(short, long)]
    value: String,
    /// TTL in seconds (optional)
    #[arg(long)]
    ttl_seconds: Option<u32>,
    /// Treat value as hex-encoded binary data
    #[arg(long)]
    hex: bool,
}

#[derive(Parser)]
struct Get {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Key to retrieve
    #[arg(short, long)]
    key: String,
    /// Output value as hex-encoded binary
    #[arg(long)]
    hex: bool,
}

#[derive(Parser)]
struct Delete {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the key
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Key to delete
    #[arg(short, long)]
    key: String,
}

#[derive(Parser)]
struct Range {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the keys
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Start key (inclusive)
    #[arg(short, long)]
    start: String,
    /// End key (exclusive)
    #[arg(short, long)]
    end: String,
    /// Maximum number of results
    #[arg(short, long)]
    limit: Option<usize>,
    /// Output values as hex-encoded binary
    #[arg(long)]
    hex: bool,
}

#[derive(Parser)]
struct Compact {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Maximum bytes to write during compaction
    #[arg(long)]
    write_budget_bytes: Option<u64>,
}

#[derive(Parser)]
struct Export {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Output file or directory
    #[arg(short, long)]
    output: PathBuf,
    /// Export format (json or binary)
    #[arg(short, long, default_value = "json")]
    format: String,
}

#[derive(Parser)]
struct Import {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Input file or directory
    #[arg(short, long)]
    input: PathBuf,
    /// Import format (json or binary)
    #[arg(short, long, default_value = "json")]
    format: String,
}

#[derive(Parser)]
struct VectorPut {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the vector
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Key to store
    #[arg(short, long)]
    key: String,
    /// Number of dimensions
    #[arg(short, long)]
    dims: u16,
    /// Data type (f32, f16, i8)
    #[arg(short, long, default_value = "f32")]
    dtype: String,
    /// Vector data (hex-encoded)
    #[arg(short, long)]
    data: String,
}

#[derive(Parser)]
struct VectorGet {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the vector
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Key to retrieve
    #[arg(short, long)]
    key: String,
}

#[derive(Parser)]
struct VectorSearch {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for the vectors
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Query vector (hex-encoded)
    #[arg(short, long)]
    query: String,
    /// Number of results to return
    #[arg(short, long, default_value = "10")]
    k: usize,
    /// Distance metric (cosine, dot, euclidean)
    #[arg(short, long, default_value = "cosine")]
    metric: String,
}

#[derive(Parser)]
struct TextSearch {
    /// Path to the database
    #[arg(short, long)]
    path: PathBuf,
    /// Namespace for text documents
    #[arg(short, long, default_value = "default")]
    namespace: String,
    /// Search query
    #[arg(short, long)]
    query: String,
    /// Maximum number of results
    #[arg(short, long, default_value = "10")]
    k: usize,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create(cmd) => handle_create(cmd),
        Commands::Stats(cmd) => handle_stats(cmd),
        Commands::Put(cmd) => handle_put(cmd),
        Commands::Get(cmd) => handle_get(cmd),
        Commands::Delete(cmd) => handle_delete(cmd),
        Commands::Range(cmd) => handle_range(cmd),
        Commands::Compact(cmd) => handle_compact(cmd),
        Commands::Export(cmd) => handle_export(cmd),
        Commands::Import(cmd) => handle_import(cmd),
        Commands::VectorPut(cmd) => handle_vector_put(cmd),
        Commands::VectorGet(cmd) => handle_vector_get(cmd),
        Commands::VectorSearch(cmd) => handle_vector_search(cmd),
        Commands::TextSearch(cmd) => handle_text_search(cmd),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn handle_create(cmd: Create) -> Result<(), Box<dyn std::error::Error>> {
    // Create directory if it doesn't exist
    std::fs::create_dir_all(&cmd.path)
        .map_err(|e| format!("Failed to create database directory: {}", e))?;
    
    // Create config with default settings
    let config = EdgestoreConfig::new(&cmd.path);
    
    // Open the engine (this creates all necessary files)
    let _engine = Engine::open(config)
        .map_err(|e| format!("Failed to create database: {}", e))?;
    
    println!("Created database at {}", cmd.path.display());
    Ok(())
}

fn handle_stats(cmd: Stats) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    
    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    // Collect statistics
    let stats = collect_stats(&cmd.path, &engine)?;
    
    if cmd.json {
        // Output as JSON
        let json = serde_json::to_string_pretty(&stats)?;
        println!("{}", json);
    } else {
        // Output as formatted table
        print_stats_table(&stats);
    }
    
    Ok(())
}

fn handle_put(cmd: Put) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    
    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    // Decode value if hex flag is set
    let value_bytes = if cmd.hex {
        hex::decode(&cmd.value)
            .map_err(|e| format!("Invalid hex value: {}", e))?
    } else {
        cmd.value.into_bytes()
    };
    
    // Store the key-value pair
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    
    if let Some(ttl) = cmd.ttl_seconds {
        engine.put_with_ttl(namespace, key, &value_bytes, ttl)
            .map_err(|e| format!("Failed to store key with TTL: {}", e))?;
        println!("Stored key '{}' with TTL {} seconds", cmd.key, ttl);
    } else {
        engine.put(namespace, key, &value_bytes)
            .map_err(|e| format!("Failed to store key: {}", e))?;
        println!("Stored key '{}'", cmd.key);
    }
    
    Ok(())
}

fn handle_get(cmd: Get) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    
    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    // Retrieve the value
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    
    match engine.get(namespace, key) {
        Ok(Some(value)) => {
            if cmd.hex {
                // Output as hex
                println!("{}", hex::encode(&value));
            } else {
                // Try to output as UTF-8, fall back to hex
                match std::str::from_utf8(&value) {
                    Ok(s) => println!("{}", s),
                    Err(_) => {
                        println!("(binary) {}", hex::encode(&value));
                    }
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

fn handle_delete(cmd: Delete) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    
    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    // Delete the key
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();
    
    engine.delete(namespace, key)
        .map_err(|e| format!("Failed to delete key: {}", e))?;
    
    println!("Deleted key '{}'", cmd.key);
    Ok(())
}

fn handle_range(cmd: Range) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    
    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    // Perform range scan
    let namespace = cmd.namespace.as_bytes();
    let start = cmd.start.as_bytes();
    let end = cmd.end.as_bytes();
    
    let results = engine.range(namespace, start, end)
        .map_err(|e| format!("Failed to perform range scan: {}", e))?;
    
    // Apply limit if specified
    let limit = cmd.limit.unwrap_or(results.len());
    let count = results.len().min(limit);
    
    // Output results
    for (key, value) in results.iter().take(limit) {
        let key_str = String::from_utf8_lossy(key);
        
        if cmd.hex {
            // Output both key and value as hex
            println!("{}={}", hex::encode(key), hex::encode(value));
        } else {
            // Try to output as UTF-8
            match std::str::from_utf8(value) {
                Ok(val_str) => println!("{}={}", key_str, val_str),
                Err(_) => println!("{}=(binary) {}", key_str, hex::encode(value)),
            }
        }
    }
    
    eprintln!("Found {} keys (showing {})", results.len(), count);
    
    Ok(())
}

#[derive(serde::Serialize)]
struct DatabaseStats {
    path: String,
    segment_count: usize,
    wal_file_count: usize,
    total_size_bytes: u64,
    metrics: MetricStats,
}

#[derive(serde::Serialize)]
struct MetricStats {
    puts: u64,
    gets: u64,
    deletes: u64,
    ranges: u64,
    compactions: u64,
    segment_flushes: u64,
    wal_rotations: u64,
}

fn collect_stats(path: &PathBuf, engine: &Engine) -> Result<DatabaseStats, Box<dyn std::error::Error>> {
    // Count WAL files
    let wal_count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name()
                .to_str()
                .map(|name| name.starts_with("wal-") && name.ends_with(".log"))
                .unwrap_or(false)
        })
        .count();
    
    // Count segment files
    let segment_count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name()
                .to_str()
                .map(|name| name.starts_with("segment-") && name.ends_with(".dat"))
                .unwrap_or(false)
        })
        .count();
    
    // Get metrics from engine
    let m = engine.metrics();
    
    // Calculate total size on disk
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

fn handle_compact(cmd: Compact) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::EdgestoreError;

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Check for lock file to provide better error message
    let lock_path = cmd.path.join("LOCK");
    if lock_path.exists() {
        // Try to open lock file to check if database is busy
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&lock_path) {
            use fs2::FileExt;
            if file.try_lock_exclusive().is_err() {
                return Err("Database is locked by another process. Please close other connections to this database first.".into());
            }
        }
    }

    // Open the engine
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

    // Get initial stats
    let initial_segments = count_segment_files(&cmd.path)?;

    // Run compaction
    let stats = engine.compact_once()
        .map_err(|e| format!("Compaction failed: {}", e))?;

    // Get final stats
    let final_segments = count_segment_files(&cmd.path)?;

    // Print results with human-readable formatting
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

fn count_segment_files(path: &PathBuf) -> Result<usize, std::io::Error> {
    let count = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name()
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

fn handle_export(cmd: Export) -> Result<(), Box<dyn std::error::Error>> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct ExportRecord {
        namespace: String,
        key: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttl: Option<u64>,
    }

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Create output file
    let output_file = std::fs::File::create(&cmd.output)
        .map_err(|e| format!("Failed to create output file: {}", e))?;
    let mut writer = std::io::BufWriter::new(output_file);

    let format = cmd.format.to_lowercase();

    if format == "json" {
        // Write opening bracket
        writer.write_all(b"[\n")?;

        // Stream all records using range scan
        let mut count = 0u64;
        let mut first = true;

        // Scan all namespaces by looking for special namespaces
        // First scan default namespace
        let results = engine.range(b"default", b"\x00", &[0xFF; 256])?;

        for (key, value) in results {
            if !first {
                writer.write_all(b",\n")?;
            }
            first = false;

            let record = ExportRecord {
                namespace: "default".to_string(),
                key: String::from_utf8_lossy(&key).to_string(),
                value: hex::encode(&value),
                ttl: None,
            };

            let json = serde_json::to_string(&record)?;
            writer.write_all(json.as_bytes())?;

            count += 1;
            if count.is_multiple_of(1000) {
                eprintln!("Exported {} keys...", count);
            }
        }

        // Write closing bracket
        writer.write_all(b"\n]\n")?;
        writer.flush()?;

        println!("Exported {} keys to {}", count, cmd.output.display());
    } else if format == "binary" {
        // Binary format: length-prefixed records
        // Format: [namespace_len: u16][namespace][key_len: u16][key][value_len: u32][value]
        let mut count = 0u64;

        let results = engine.range(b"default", b"\x00", &[0xFF; 256])?;

        for (key, value) in results {
            // Write namespace
            let ns = b"default";
            writer.write_all(&(ns.len() as u16).to_le_bytes())?;
            writer.write_all(ns)?;

            // Write key
            writer.write_all(&(key.len() as u16).to_le_bytes())?;
            writer.write_all(&key)?;

            // Write value
            writer.write_all(&(value.len() as u32).to_le_bytes())?;
            writer.write_all(&value)?;

            count += 1;
            if count.is_multiple_of(1000) {
                eprintln!("Exported {} keys...", count);
            }
        }

        writer.flush()?;
        println!("Exported {} keys to {} (binary format)", count, cmd.output.display());
    } else {
        return Err(format!("Unknown format: {}. Use 'json' or 'binary'.", cmd.format).into());
    }

    Ok(())
}

fn handle_import(cmd: Import) -> Result<(), Box<dyn std::error::Error>> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ImportRecord {
        namespace: String,
        key: String,
        value: String,
        #[serde(default)]
        #[allow(dead_code)]
        ttl: Option<u64>,
    }

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Verify input file exists
    if !cmd.input.exists() {
        return Err(format!("Input file does not exist: {}", cmd.input.display()).into());
    }

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    let format = cmd.format.to_lowercase();

    if format == "json" {
        // Read and parse JSON
        let input_data = std::fs::read_to_string(&cmd.input)
            .map_err(|e| format!("Failed to read input file: {}", e))?;

        let records: Vec<ImportRecord> = serde_json::from_str(&input_data)
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let total = records.len();
        let mut count = 0u64;

        for record in records {
            let namespace = record.namespace.as_bytes();
            let key = record.key.as_bytes();
            let value = hex::decode(&record.value)
                .map_err(|e| format!("Invalid hex value for key '{}': {}", record.key, e))?;

            engine.put(namespace, key, &value)
                .map_err(|e| format!("Failed to store key '{}': {}", record.key, e))?;

            count += 1;
            if count.is_multiple_of(1000) {
                eprintln!("Imported {}/{} keys...", count, total);
            }
        }

        println!("Imported {} keys from {} (JSON format)", count, cmd.input.display());
    } else if format == "binary" {
        // Binary format: length-prefixed records
        let input_data = std::fs::read(&cmd.input)
            .map_err(|e| format!("Failed to read input file: {}", e))?;

        let mut offset = 0usize;
        let mut count = 0u64;

        while offset < input_data.len() {
            // Read namespace length
            if offset + 2 > input_data.len() {
                break;
            }
            let ns_len = u16::from_le_bytes([input_data[offset], input_data[offset + 1]]) as usize;
            offset += 2;

            // Read namespace
            if offset + ns_len > input_data.len() {
                break;
            }
            let namespace = &input_data[offset..offset + ns_len];
            offset += ns_len;

            // Read key length
            if offset + 2 > input_data.len() {
                break;
            }
            let key_len = u16::from_le_bytes([input_data[offset], input_data[offset + 1]]) as usize;
            offset += 2;

            // Read key
            if offset + key_len > input_data.len() {
                break;
            }
            let key = &input_data[offset..offset + key_len];
            offset += key_len;

            // Read value length
            if offset + 4 > input_data.len() {
                break;
            }
            let value_len = u32::from_le_bytes([
                input_data[offset],
                input_data[offset + 1],
                input_data[offset + 2],
                input_data[offset + 3],
            ]) as usize;
            offset += 4;

            // Read value
            if offset + value_len > input_data.len() {
                break;
            }
            let value = &input_data[offset..offset + value_len];
            offset += value_len;

            // Store the record
            engine.put(namespace, key, value)
                .map_err(|e| format!("Failed to store record: {}", e))?;

            count += 1;
            if count.is_multiple_of(1000) {
                eprintln!("Imported {} keys...", count);
            }
        }

        println!("Imported {} keys from {} (binary format)", count, cmd.input.display());
    } else {
        return Err(format!("Unknown format: {}. Use 'json' or 'binary'.", cmd.format).into());
    }

    Ok(())
}

fn handle_vector_put(cmd: VectorPut) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::{VectorEngine, vector::types::Dtype};

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Parse dtype
    let dtype = match cmd.dtype.to_lowercase().as_str() {
        "f32" => Dtype::F32,
        "f16" => Dtype::F16,
        "i8" => Dtype::I8,
        _ => return Err(format!("Unknown dtype: {}. Use 'f32', 'f16', or 'i8'.", cmd.dtype).into()),
    };

    // Decode hex data
    let data = hex::decode(&cmd.data)
        .map_err(|e| format!("Invalid hex data: {}", e))?;

    // Validate data length
    let expected_len = cmd.dims as usize * dtype.element_size();
    if data.len() != expected_len {
        return Err(format!(
            "Data length mismatch: expected {} bytes for {} dims of {:?}, got {}",
            expected_len, cmd.dims, dtype, data.len()
        ).into());
    }

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Store the vector
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();

    engine.vector_put(namespace, key, cmd.dims, dtype, &data)
        .map_err(|e| format!("Failed to store vector: {}", e))?;

    println!("Stored vector '{}' ({} dims, {:?})", cmd.key, cmd.dims, dtype);
    Ok(())
}

fn handle_vector_get(cmd: VectorGet) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::VectorEngine;

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Retrieve the vector
    let namespace = cmd.namespace.as_bytes();
    let key = cmd.key.as_bytes();

    match engine.vector_get(namespace, key) {
        Ok(Some(record)) => {
            println!("Vector '{}' found:", cmd.key);
            println!("  Dimensions: {}", record.dims);
            println!("  Data type: {:?}", record.dtype);
            println!("  Data (hex): {}", hex::encode(&record.data));
            Ok(())
        }
        Ok(None) => {
            eprintln!("Vector not found: {}", cmd.key);
            std::process::exit(1);
        }
        Err(e) => Err(format!("Failed to retrieve vector: {}", e).into()),
    }
}

fn handle_vector_search(cmd: VectorSearch) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::{vector::types::{Dtype, VectorRecord}, vector::distance::Metric};

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Parse metric
    let metric = match cmd.metric.to_lowercase().as_str() {
        "cosine" => Metric::Cosine,
        "euclidean" | "l2" => Metric::L2,
        "dot" | "dotproduct" => Metric::DotProduct,
        _ => return Err(format!("Unknown metric: {}. Use 'cosine', 'euclidean', or 'dot'.", cmd.metric).into()),
    };

    // Decode query vector
    let query_data = hex::decode(&cmd.query)
        .map_err(|e| format!("Invalid hex query data: {}", e))?;

    // Infer dimensions from data length (assume f32 if not specified)
    // For now, we require data length to be divisible by 4 (f32)
    if query_data.len() % 4 != 0 {
        return Err("Query vector data length must be divisible by 4 (assuming f32)".into());
    }
    let dims = (query_data.len() / 4) as u16;

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Build query record
    let query = VectorRecord {
        dims,
        dtype: Dtype::F32,
        data: query_data,
    };

    // Perform search
    let namespace = cmd.namespace.as_bytes();
    let results = engine.vector_search(namespace, &query, cmd.k, metric)
        .map_err(|e| format!("Search failed: {}", e))?;

    if results.is_empty() {
        println!("No matching vectors found.");
    } else {
        println!("Top {} nearest vectors (using {:?} metric):", results.len(), metric);
        for (i, result) in results.iter().enumerate() {
            let key_str = String::from_utf8_lossy(&result.key);
            println!("  {}. {} = {:.6}", i + 1, key_str, result.distance);
        }
    }

    Ok(())
}

fn handle_text_search(cmd: TextSearch) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::TextEngine;

    // Verify the path exists
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }

    // Open the engine
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // Perform search
    let namespace = cmd.namespace.as_bytes();
    let results = engine.search_text(namespace, &cmd.query, cmd.k)
        .map_err(|e| format!("Search failed: {}", e))?;

    if results.is_empty() {
        println!("No matching documents found for query: '{}'", cmd.query);
    } else {
        println!("Top {} documents for query '{}' (BM25 ranked):", results.len(), cmd.query);
        for (i, result) in results.iter().enumerate() {
            let key_str = String::from_utf8_lossy(&result.doc_id);
            println!("  {}. {} = {:.4}", i + 1, key_str, result.score);
        }
    }

    Ok(())
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
