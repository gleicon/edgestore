use clap::Parser;
use edgestore::{EdgestoreConfig, Engine};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
pub struct Export {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Output file or directory
    #[arg(short, long)]
    pub output: PathBuf,
    /// Export format (json or binary)
    #[arg(short, long, default_value = "json")]
    pub format: String,
}

#[derive(Parser)]
pub struct Import {
    /// Path to the database
    #[arg(short, long)]
    pub path: PathBuf,
    /// Input file or directory
    #[arg(short, long)]
    pub input: PathBuf,
    /// Import format (json or binary)
    #[arg(short, long, default_value = "json")]
    pub format: String,
}

pub fn handle_export(cmd: Export) -> Result<(), Box<dyn std::error::Error>> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct ExportRecord {
        namespace: String,
        key: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttl: Option<u64>,
    }

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let output_file = std::fs::File::create(&cmd.output)
        .map_err(|e| format!("Failed to create output file: {}", e))?;
    let mut writer = std::io::BufWriter::new(output_file);
    let format = cmd.format.to_lowercase();

    if format == "json" {
        writer.write_all(b"[\n")?;
        let mut count = 0u64;
        let mut first = true;
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
        writer.write_all(b"\n]\n")?;
        writer.flush()?;
        println!("Exported {} keys to {}", count, cmd.output.display());
    } else if format == "binary" {
        let mut count = 0u64;
        let results = engine.range(b"default", b"\x00", &[0xFF; 256])?;
        for (key, value) in results {
            let ns = b"default";
            writer.write_all(&(ns.len() as u16).to_le_bytes())?;
            writer.write_all(ns)?;
            writer.write_all(&(key.len() as u16).to_le_bytes())?;
            writer.write_all(&key)?;
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

pub fn handle_import(cmd: Import) -> Result<(), Box<dyn std::error::Error>> {
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

    if !cmd.path.exists() {
        return Err(format!("Database path does not exist: {}", cmd.path.display()).into());
    }
    if !cmd.input.exists() {
        return Err(format!("Input file does not exist: {}", cmd.input.display()).into());
    }
    let config = EdgestoreConfig::new(&cmd.path);
    let mut engine = Engine::open(config).map_err(|e| format!("Failed to open database: {}", e))?;
    let format = cmd.format.to_lowercase();

    if format == "json" {
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
            engine
                .put(namespace, key, &value)
                .map_err(|e| format!("Failed to store key '{}': {}", record.key, e))?;
            count += 1;
            if count.is_multiple_of(1000) {
                eprintln!("Imported {}/{} keys...", count, total);
            }
        }
        println!("Imported {} keys from {} (JSON format)", count, cmd.input.display());
    } else if format == "binary" {
        let input_data =
            std::fs::read(&cmd.input).map_err(|e| format!("Failed to read input file: {}", e))?;
        let mut offset = 0usize;
        let mut count = 0u64;
        while offset < input_data.len() {
            if offset + 2 > input_data.len() {
                break;
            }
            let ns_len = u16::from_le_bytes([input_data[offset], input_data[offset + 1]]) as usize;
            offset += 2;
            if offset + ns_len > input_data.len() {
                break;
            }
            let namespace = &input_data[offset..offset + ns_len];
            offset += ns_len;
            if offset + 2 > input_data.len() {
                break;
            }
            let key_len = u16::from_le_bytes([input_data[offset], input_data[offset + 1]]) as usize;
            offset += 2;
            if offset + key_len > input_data.len() {
                break;
            }
            let key = &input_data[offset..offset + key_len];
            offset += key_len;
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
            if offset + value_len > input_data.len() {
                break;
            }
            let value = &input_data[offset..offset + value_len];
            offset += value_len;
            engine
                .put(namespace, key, value)
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
