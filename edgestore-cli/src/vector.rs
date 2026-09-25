use clap::Parser;
use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

#[derive(Parser)]
pub struct VectorPut {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the vector
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Key to store
    #[arg(short, long)]
    pub key: String,
    /// Number of dimensions
    #[arg(short, long)]
    pub dims: u16,
    /// Data type (f32, f16, i8)
    #[arg(short, long, default_value = "f32")]
    pub dtype: String,
    /// Vector data (hex-encoded)
    #[arg(short, long)]
    pub data: String,
}

#[derive(Parser)]
pub struct VectorGet {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the vector
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Key to retrieve
    #[arg(short, long)]
    pub key: String,
}

#[derive(Parser)]
pub struct VectorSearch {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Namespace for the vectors
    #[arg(short, long, default_value = "default")]
    pub namespace: String,
    /// Query vector (hex-encoded)
    #[arg(short, long)]
    pub query: String,
    /// Number of results to return
    #[arg(short, long, default_value = "10")]
    pub k: usize,
    /// Distance metric (cosine, dot, euclidean)
    #[arg(short, long, default_value = "cosine")]
    pub metric: String,
}

pub fn handle_vector_put(cmd: VectorPut) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::{vector::types::Dtype, VectorEngine};

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let dtype = match cmd.dtype.to_lowercase().as_str() {
        "f32" => Dtype::F32,
        "f16" => Dtype::F16,
        "i8" => Dtype::I8,
        _ => return Err(format!("Unknown dtype: {}. Use 'f32', 'f16', or 'i8'.", cmd.dtype).into()),
    };
    let data = hex::decode(&cmd.data).map_err(|e| format!("Invalid hex data: {}", e))?;
    let expected_len = cmd.dims as usize * dtype.element_size();
    if data.len() != expected_len {
        return Err(format!(
            "Data length mismatch: expected {} bytes for {} dims of {:?}, got {}",
            expected_len, cmd.dims, dtype, data.len()
        )
        .into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    engine
        .vector_put(cmd.namespace.as_bytes(), cmd.key.as_bytes(), cmd.dims, dtype, &data)
        .map_err(|e| format!("Failed to store vector: {}", e))?;
    println!("Stored vector '{}' ({} dims, {:?})", cmd.key, cmd.dims, dtype);
    Ok(())
}

pub fn handle_vector_get(cmd: VectorGet) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::VectorEngine;

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    match engine.vector_get(cmd.namespace.as_bytes(), cmd.key.as_bytes()) {
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

pub fn handle_vector_search(cmd: VectorSearch) -> Result<(), Box<dyn std::error::Error>> {
    use edgestore::{
        vector::distance::Metric,
        vector::types::{Dtype, VectorRecord},
    };

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let metric = match cmd.metric.to_lowercase().as_str() {
        "cosine" => Metric::Cosine,
        "euclidean" | "l2" => Metric::L2,
        "dot" | "dotproduct" => Metric::DotProduct,
        _ => {
            return Err(format!(
                "Unknown metric: {}. Use 'cosine', 'euclidean', or 'dot'.",
                cmd.metric
            )
            .into())
        }
    };
    let query_data =
        hex::decode(&cmd.query).map_err(|e| format!("Invalid hex query data: {}", e))?;
    if query_data.len() % 4 != 0 {
        return Err("Query vector data length must be divisible by 4 (assuming f32)".into());
    }
    let dims = (query_data.len() / 4) as u16;
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let query = VectorRecord {
        dims,
        dtype: Dtype::F32,
        data: query_data,
    };
    let results = engine
        .vector_search(cmd.namespace.as_bytes(), &query, cmd.k, metric)
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
