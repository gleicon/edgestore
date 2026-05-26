use clap::{Parser, Subcommand};
use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

/// EdgeStore CLI — Administrative tool for managing EdgeStore databases
#[derive(Parser)]
#[command(name = "edgestore-cli")]
#[command(about = "EdgeStore database administration tool")]
#[command(version = "1.0.0")]
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

fn main() {
    let cli = Cli::parse();
    
    let result = match cli.command {
        Commands::Create(cmd) => handle_create(cmd),
        Commands::Stats(cmd) => handle_stats(cmd),
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
