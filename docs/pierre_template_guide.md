# Log Template Extraction (Pierre guide)

**ENG-10 is a caller-side pattern, not an engine API.**

Template extraction — finding that `"Error at line 42: disk full"` and
`"Error at line 7: disk full"` share the shape `"Error at line {N}: disk full"` —
is log-analysis domain logic that belongs in Pierre's pipeline, not in edgestore's
compaction pass. edgestore stores and retrieves bytes; what those bytes mean is
the application's concern.

## Design

Store three kinds of records in edgestore, all in the same namespace:

| Key pattern | Value | Purpose |
|---|---|---|
| `log:{id}` | raw log line | Source of truth |
| `tpl:{template_id}` | template string with `{var}` placeholders | One entry per shape |
| `bind:{id}` | MessagePack: `{template_id, vars: {N: "42", ...}}` | Links a log to its template |

This lets you:
- Retrieve any log line by ID (`get(ns, "log:{id}")`)
- Find all logs matching a template (`prefix(ns, "bind:{template_id}:")`)
- Enumerate all known shapes (`prefix(ns, "tpl:")`)
- Delete a template and its bindings together in one namespace sweep

## Implementing template extraction

```rust
use std::collections::HashMap;

/// Replace variable tokens with `{N}` placeholders and collect their values.
/// A "variable token" is any token that differs between two otherwise-identical
/// log lines. In practice, use a regex or a simple heuristic (pure-numeric tokens,
/// hex strings, timestamps, UUIDs).
pub fn extract_template(line: &str) -> (String, HashMap<String, String>) {
    let mut template_parts = Vec::new();
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut var_idx = 0usize;

    for token in line.split_whitespace() {
        if is_variable(token) {
            let placeholder = format!("{{{}}}", var_idx);
            vars.insert(var_idx.to_string(), token.to_string());
            template_parts.push(placeholder);
            var_idx += 1;
        } else {
            template_parts.push(token.to_string());
        }
    }

    (template_parts.join(" "), vars)
}

fn is_variable(token: &str) -> bool {
    // Numeric, hex, UUID, timestamp, file path, IP address — anything that varies.
    token.chars().all(|c| c.is_ascii_digit())
        || token.starts_with("0x")
        || token.len() == 36 && token.chars().filter(|&c| c == '-').count() == 4
}
```

## Storing templates and bindings in edgestore

```rust
use edgestore::{Engine, EdgestoreConfig};
use std::collections::HashMap;

fn ingest_log_line(engine: &mut Engine, ns: &[u8], log_id: &str, line: &str) {
    // 1. Store the raw line.
    engine.put(ns, format!("log:{}", log_id).as_bytes(), line.as_bytes()).unwrap();

    // 2. Extract the template.
    let (template, vars) = extract_template(line);

    // 3. Derive a stable template ID (hash of the template string).
    let template_id = format!("{:x}", blake3::hash(template.as_bytes()));

    // 4. Store the template shape (idempotent — same template_id = same value).
    let tpl_key = format!("tpl:{}", template_id);
    if engine.get(ns, tpl_key.as_bytes()).unwrap().is_none() {
        engine.put(ns, tpl_key.as_bytes(), template.as_bytes()).unwrap();
    }

    // 5. Store the variable bindings for this specific log line.
    let bind_key = format!("bind:{}:{}", template_id, log_id);
    let binding_bytes = serde_json::to_vec(&vars).unwrap();
    engine.put(ns, bind_key.as_bytes(), &binding_bytes).unwrap();
}
```

## Querying by template

```rust
/// All log IDs that match a given template shape.
fn logs_for_template(engine: &mut Engine, ns: &[u8], template_id: &str) -> Vec<String> {
    let prefix = format!("bind:{}:", template_id);
    engine
        .prefix(ns, prefix.as_bytes())
        .unwrap()
        .into_iter()
        .map(|(k, _)| {
            // key = "bind:{template_id}:{log_id}" — strip the prefix
            String::from_utf8(k[prefix.len()..].to_vec()).unwrap()
        })
        .collect()
}

/// Find anomalies: template shapes that appeared in the last window but not before.
fn new_templates_since(
    engine: &mut Engine,
    ns: &[u8],
    since_log_id: &str,
) -> Vec<(String, String)> { // (template_id, template_string)
    engine
        .prefix(ns, b"tpl:")
        .unwrap()
        .into_iter()
        .filter(|(k, _)| {
            // A real implementation would check the creation timestamp or a counter
            // stored alongside each template. This is a structural sketch.
            let tpl_id = &k[4..]; // strip "tpl:"
            let prefix = format!("bind:{}:", String::from_utf8_lossy(tpl_id));
            // If ANY binding key > since_log_id exists, it's "new".
            engine
                .prefix(ns, prefix.as_bytes())
                .unwrap()
                .into_iter()
                .any(|(bk, _)| bk.as_slice() > since_log_id.as_bytes())
        })
        .map(|(k, v)| {
            let tpl_id = String::from_utf8(k[4..].to_vec()).unwrap();
            let tpl_str = String::from_utf8(v).unwrap_or_default();
            (tpl_id, tpl_str)
        })
        .collect()
}
```

## Token reduction for agent output

When an agent needs to read a log entry, return the template + variable bindings
instead of the full log line. This is a 3-10× token reduction for repetitive logs:

```rust
fn agent_read(engine: &mut Engine, ns: &[u8], log_id: &str) -> String {
    // Try to return compact (template + vars) form.
    let bind_prefix = format!("bind:");
    let bind_results = engine.prefix(ns, bind_prefix.as_bytes()).unwrap();

    for (k, v) in bind_results {
        let key_str = String::from_utf8_lossy(&k);
        // key = "bind:{template_id}:{log_id}"
        if key_str.ends_with(&format!(":{}", log_id)) {
            let parts: Vec<&str> = key_str.split(':').collect();
            if parts.len() >= 3 {
                let template_id = parts[1];
                let tpl_key = format!("tpl:{}", template_id);
                if let Ok(Some(tpl_bytes)) = engine.get(ns, tpl_key.as_bytes()) {
                    let template = String::from_utf8_lossy(&tpl_bytes);
                    let vars: HashMap<String, String> = serde_json::from_slice(&v).unwrap_or_default();
                    return format!("template: {}\nvars: {:?}", template, vars);
                }
            }
        }
    }

    // Fall back to raw log line.
    engine
        .get(ns, format!("log:{}", log_id).as_bytes())
        .unwrap()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}
```

## At compaction time

The pattern above runs at ingest time (cheap, incremental). If you want to run
template extraction as a post-compaction pass instead:

1. After `engine.flush_to_segments()`, call `engine.range(ns, b"log:", b"log:\xFF")`
   to enumerate recent raw log lines.
2. Run `extract_template` on each.
3. Write the `tpl:` and `bind:` records back into the engine.
4. Call `engine.flush_to_segments()` again to persist the template index.

This is the equivalent of ENG-10's "compaction-time pass" — but running in your
application code, not inside edgestore's compactor, which is the right boundary.
