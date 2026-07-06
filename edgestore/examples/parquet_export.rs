//! Export an EdgeStore namespace to a Parquet file via `ImmutableEngine`.
//!
//! This example shows how to use `ImmutableEngine` as a bridge between EdgeStore
//! segment bytes and analytics tools (DuckDB, DataFusion, Spark). The schema is
//! caller-defined — EdgeStore stores opaque bytes; you decide the columns.
//!
//! EdgeStore does not own the schema. This example assumes records in the `"logs"`
//! namespace are newline-free UTF-8 strings (a common log line format). Adjust
//! the schema and deserialization for your own payload format.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example parquet_export -- /var/db logs /tmp/logs.parquet
//! ```
//!
//! ## Reading from DuckDB
//!
//! ```sql
//! SELECT * FROM '/tmp/logs.parquet' LIMIT 10;
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::{BinaryArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use edgestore::{EdgestoreConfig, Engine};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("Usage: parquet_export <db_path> <namespace> <output.parquet>");
        std::process::exit(1);
    }
    let db_path = PathBuf::from(&args[1]);
    let namespace = args[2].as_bytes();
    let out_path = PathBuf::from(&args[3]);

    // Open read-only via Engine (no writes during export).
    let engine = Engine::open(EdgestoreConfig::new(&db_path))?;

    // Scan the full namespace.
    let records = engine.range(namespace, b"", b"\xff")?;

    if records.is_empty() {
        println!("No records found in namespace {:?}", args[2]);
        return Ok(());
    }

    // Define a two-column schema: key (binary) + value (utf8).
    // Change this to match your payload format.
    let schema = Arc::new(Schema::new(vec![
        Field::new("key", DataType::Binary, false),
        Field::new("value", DataType::Utf8, true),
    ]));

    // Split records into column buffers.
    let mut keys: Vec<&[u8]> = Vec::with_capacity(records.len());
    let mut values: Vec<Option<&str>> = Vec::with_capacity(records.len());

    for (k, v) in &records {
        keys.push(k.as_slice());
        // Best-effort UTF-8 decode; null if value is binary.
        values.push(std::str::from_utf8(v.as_slice()).ok());
    }

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(BinaryArray::from(keys)),
            Arc::new(StringArray::from(values)),
        ],
    )?;

    // Write Parquet with zstd compression.
    let file = std::fs::File::create(&out_path)?;
    let props = WriterProperties::builder()
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;

    println!(
        "Exported {} records from namespace {:?} to {}",
        records.len(),
        args[2],
        out_path.display()
    );
    Ok(())
}
