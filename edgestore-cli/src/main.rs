mod kv;
mod replication;
mod text;
mod vector;

use clap::{Parser, Subcommand};

/// EdgeStore CLI — Administrative tool for managing EdgeStore databases
#[derive(Parser)]
#[command(name = "edgestore-cli")]
#[command(about = "EdgeStore database administration tool")]
#[command(version = "2.0.3")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new EdgeStore database
    Create(kv::Create),
    /// Show database statistics
    Stats(kv::Stats),
    /// Store a key-value pair
    Put(kv::Put),
    /// Retrieve a value by key
    Get(kv::Get),
    /// Delete a key
    Delete(kv::Delete),
    /// List keys in a range
    Range(kv::Range),
    /// Run compaction to reclaim space
    Compact(kv::Compact),
    /// Export database to file
    Export(replication::Export),
    /// Import database from file
    Import(replication::Import),
    /// Store a vector
    VectorPut(vector::VectorPut),
    /// Retrieve a vector by key
    VectorGet(vector::VectorGet),
    /// Search for similar vectors
    VectorSearch(vector::VectorSearch),
    /// Search text documents
    TextSearch(text::TextSearch),
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create(cmd) => kv::handle_create(cmd),
        Commands::Stats(cmd) => kv::handle_stats(cmd),
        Commands::Put(cmd) => kv::handle_put(cmd),
        Commands::Get(cmd) => kv::handle_get(cmd),
        Commands::Delete(cmd) => kv::handle_delete(cmd),
        Commands::Range(cmd) => kv::handle_range(cmd),
        Commands::Compact(cmd) => kv::handle_compact(cmd),
        Commands::Export(cmd) => replication::handle_export(cmd),
        Commands::Import(cmd) => replication::handle_import(cmd),
        Commands::VectorPut(cmd) => vector::handle_vector_put(cmd),
        Commands::VectorGet(cmd) => vector::handle_vector_get(cmd),
        Commands::VectorSearch(cmd) => vector::handle_vector_search(cmd),
        Commands::TextSearch(cmd) => text::handle_text_search(cmd),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
