use clap::Parser;
use edgestore::{EdgestoreConfig, Engine};
use edgestore_text::TextIndex;
use std::path::PathBuf;

#[derive(Parser)]
pub struct TextSearch {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for text documents
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Search query
    #[arg(short, long)]
    pub query: String,
    /// Maximum number of results
    #[arg(short, long, default_value = "10")]
    pub k: usize,
}

pub fn handle_text_search(cmd: TextSearch) -> Result<(), Box<dyn std::error::Error>> {
    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let mut text = TextIndex::new();
    let results = text
        .search(&engine, cmd.namespace.as_bytes(), &cmd.query, cmd.k)
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
