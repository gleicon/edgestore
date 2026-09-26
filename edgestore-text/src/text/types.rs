use std::collections::HashMap;

/// A facet value for filtering search results.
#[derive(Debug, Clone, PartialEq)]
pub enum FacetValue {
    /// String facet value.
    String(String),
    /// Integer facet value.
    Number(i64),
    /// Boolean facet value.
    Bool(bool),
}

/// A text document with optional facet values.
#[derive(Debug, Clone)]
pub struct TextRecord {
    /// Raw text content.
    pub text: String,
    /// Facet values for filtering.
    pub facets: HashMap<String, FacetValue>,
}

impl TextRecord {
    /// Create a new text record with no facets.
    pub fn new(text: impl Into<String>) -> Self {
        TextRecord {
            text: text.into(),
            facets: HashMap::new(),
        }
    }

    /// Add a facet value, returning self for chaining.
    pub fn with_facet(mut self, key: impl Into<String>, value: FacetValue) -> Self {
        self.facets.insert(key.into(), value);
        self
    }
}

/// Encode a TextRecord to bytes for KV storage.
///
/// Format: `text_len:u32` (little-endian) + `text_bytes` +
///         `facet_count:u16` + per_facet: `key_len:u16` + `key_bytes` + `value_tag:u8` + value
///
/// Value tags: 0 = String (str_len:u16 + bytes), 1 = Number (i64-le), 2 = Bool (u8)
pub fn encode_text_record(record: &TextRecord) -> Vec<u8> {
    let mut buf = Vec::new();
    let text_bytes = record.text.as_bytes();
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(text_bytes);
    buf.extend_from_slice(&(record.facets.len() as u16).to_le_bytes());
    for (key, value) in &record.facets {
        let key_bytes = key.as_bytes();
        buf.extend_from_slice(&(key_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(key_bytes);
        match value {
            FacetValue::String(s) => {
                buf.push(0u8);
                let s_bytes = s.as_bytes();
                buf.extend_from_slice(&(s_bytes.len() as u16).to_le_bytes());
                buf.extend_from_slice(s_bytes);
            }
            FacetValue::Number(n) => {
                buf.push(1u8);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            FacetValue::Bool(b) => {
                buf.push(2u8);
                buf.push(if *b { 1u8 } else { 0u8 });
            }
        }
    }
    buf
}

/// Decode bytes into a TextRecord.
pub fn decode_text_record(bytes: &[u8]) -> Option<TextRecord> {
    if bytes.len() < 4 {
        return None;
    }
    let mut pos = 0usize;

    macro_rules! read {
        ($n:expr) => {{
            if bytes.len() < pos + $n {
                return None;
            }
            let slice = &bytes[pos..pos + $n];
            pos += $n;
            slice
        }};
    }

    let text_len = u32::from_le_bytes(read!(4).try_into().unwrap()) as usize;
    if bytes.len() < pos + text_len {
        return None;
    }
    let text = String::from_utf8(read!(text_len).to_vec()).ok()?;

    let facet_count = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
    let mut facets = HashMap::with_capacity(facet_count);
    for _ in 0..facet_count {
        let key_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
        let key = String::from_utf8(read!(key_len).to_vec()).ok()?;
        let tag = read!(1)[0];
        let value = match tag {
            0 => {
                let s_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
                let s = String::from_utf8(read!(s_len).to_vec()).ok()?;
                FacetValue::String(s)
            }
            1 => {
                let n = i64::from_le_bytes(read!(8).try_into().unwrap());
                FacetValue::Number(n)
            }
            2 => {
                let b = read!(1)[0] != 0;
                FacetValue::Bool(b)
            }
            _ => return None,
        };
        facets.insert(key, value);
    }

    Some(TextRecord { text, facets })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_record_roundtrip() {
        let rec = TextRecord::new("hello world")
            .with_facet("category", FacetValue::String("news".to_string()))
            .with_facet("views", FacetValue::Number(42))
            .with_facet("published", FacetValue::Bool(true));
        let encoded = encode_text_record(&rec);
        let decoded = decode_text_record(&encoded).unwrap();
        assert_eq!(decoded.text, "hello world");
        assert_eq!(
            decoded.facets.get("category"),
            Some(&FacetValue::String("news".to_string()))
        );
        assert_eq!(decoded.facets.get("views"), Some(&FacetValue::Number(42)));
        assert_eq!(
            decoded.facets.get("published"),
            Some(&FacetValue::Bool(true))
        );
    }

    #[test]
    fn test_text_record_empty_facets() {
        let rec = TextRecord::new("simple text");
        let encoded = encode_text_record(&rec);
        let decoded = decode_text_record(&encoded).unwrap();
        assert_eq!(decoded.text, "simple text");
        assert!(decoded.facets.is_empty());
    }

    #[test]
    fn test_decode_too_short() {
        assert!(decode_text_record(b"\x00").is_none());
    }
}
