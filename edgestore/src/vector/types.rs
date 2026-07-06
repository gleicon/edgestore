use crate::error::EdgestoreError;

/// Data type for vector elements.
///
/// ## Scalar quantization (SQ8)
///
/// `Dtype::I8` implements Scalar Quantization 8-bit (SQ8). Caller quantizes
/// `f32` values to `i8` before calling `vector_put`; EdgeStore stores and
/// searches the quantized bytes with no further transformation.
///
/// Typical quantization: map the range `[min, max]` of each dimension to
/// `[-128, 127]`:
///
/// ```rust,ignore
/// fn quantize_sq8(floats: &[f32]) -> Vec<u8> {
///     let min = floats.iter().cloned().fold(f32::INFINITY, f32::min);
///     let max = floats.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
///     let scale = if max == min { 1.0 } else { 255.0 / (max - min) };
///     floats.iter().map(|&v| {
///         let q = ((v - min) * scale - 128.0).clamp(-128.0, 127.0) as i8;
///         q as u8  // reinterpret bits
///     }).collect()
/// }
/// ```
///
/// **Trade-off**: I8 storage uses 4× less memory and disk than F32. Distance
/// computations widen to `i32` before accumulating, preserving precision. Recall
/// loss is typically < 1% for normalized embeddings; higher for non-normalized data.
///
/// `F16` halves storage vs F32 with minimal recall impact (< 0.1% for most
/// embedding models). Product Quantization (PQ) codebooks are **not** supported —
/// caller-side SQ8 via `Dtype::I8` is the supported quantization path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dtype {
    /// 32-bit floating point (4 bytes per element).
    F32 = 0,
    /// 16-bit floating point (2 bytes per element).
    F16 = 1,
    /// 8-bit signed integer (1 byte per element). See type-level docs for SQ8 usage.
    I8 = 2,
}

impl Dtype {
    /// Return the size of each element in bytes.
    pub fn element_size(&self) -> usize {
        match self {
            Dtype::F32 => 4,
            Dtype::F16 => 2,
            Dtype::I8 => 1,
        }
    }
}

impl TryFrom<u8> for Dtype {
    type Error = EdgestoreError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Dtype::F32),
            1 => Ok(Dtype::F16),
            2 => Ok(Dtype::I8),
            _ => Err(EdgestoreError::InvalidOperation(format!(
                "unknown dtype value: {}",
                value
            ))),
        }
    }
}

/// A vector record with dimension count, data type, and raw element bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorRecord {
    /// Number of dimensions.
    pub dims: u16,
    /// Element data type (F32, F16, or I8).
    pub dtype: Dtype,
    /// Raw element bytes (length = dims * dtype.element_size()).
    pub data: Vec<u8>,
}

/// Encode a VectorRecord into bytes: {dims:u16}{dtype:u8}{data}.
///
/// Uses big-endian for dims to match project conventions.
pub fn encode_vector_record(record: &VectorRecord) -> Result<Vec<u8>, EdgestoreError> {
    let expected = record.dims as usize * record.dtype.element_size();
    if record.data.len() != expected {
        return Err(EdgestoreError::DimensionMismatch {
            expected,
            actual: record.data.len(),
        });
    }

    let mut out = Vec::with_capacity(3 + record.data.len());
    out.extend_from_slice(&record.dims.to_be_bytes());
    out.push(record.dtype as u8);
    out.extend_from_slice(&record.data);
    Ok(out)
}

/// Decode bytes into a VectorRecord.
///
/// Format: {dims:u16}{dtype:u8}{data}.
/// Validates that data length matches dims * element_size.
pub fn decode_vector_record(bytes: &[u8]) -> Result<VectorRecord, EdgestoreError> {
    if bytes.len() < 3 {
        return Err(EdgestoreError::CorruptData(
            "vector record too short".to_string(),
        ));
    }

    let dims = u16::from_be_bytes([bytes[0], bytes[1]]);
    let dtype = Dtype::try_from(bytes[2])?;
    let data = &bytes[3..];

    let expected = dims as usize * dtype.element_size();
    if data.len() != expected {
        return Err(EdgestoreError::DimensionMismatch {
            expected,
            actual: data.len(),
        });
    }

    Ok(VectorRecord {
        dims,
        dtype,
        data: data.to_vec(),
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtype_element_size() {
        assert_eq!(Dtype::F32.element_size(), 4);
        assert_eq!(Dtype::F16.element_size(), 2);
        assert_eq!(Dtype::I8.element_size(), 1);
    }

    #[test]
    fn test_dtype_try_from() {
        assert_eq!(Dtype::try_from(0).unwrap(), Dtype::F32);
        assert_eq!(Dtype::try_from(1).unwrap(), Dtype::F16);
        assert_eq!(Dtype::try_from(2).unwrap(), Dtype::I8);
        assert!(Dtype::try_from(3).is_err());
    }

    #[test]
    fn test_encode_decode_f32_roundtrip() {
        let record = VectorRecord {
            dims: 128,
            dtype: Dtype::F32,
            data: vec![0xAB; 128 * 4],
        };
        let encoded = encode_vector_record(&record).unwrap();
        let decoded = decode_vector_record(&encoded).unwrap();
        assert_eq!(decoded.dims, 128);
        assert_eq!(decoded.dtype, Dtype::F32);
        assert_eq!(decoded.data, record.data);
    }

    #[test]
    fn test_encode_decode_f16_roundtrip() {
        let record = VectorRecord {
            dims: 64,
            dtype: Dtype::F16,
            data: vec![0xCD; 64 * 2],
        };
        let encoded = encode_vector_record(&record).unwrap();
        let decoded = decode_vector_record(&encoded).unwrap();
        assert_eq!(decoded.dims, 64);
        assert_eq!(decoded.dtype, Dtype::F16);
        assert_eq!(decoded.data, record.data);
    }

    #[test]
    fn test_encode_decode_i8_roundtrip() {
        let record = VectorRecord {
            dims: 256,
            dtype: Dtype::I8,
            data: vec![0xEF; 256],
        };
        let encoded = encode_vector_record(&record).unwrap();
        let decoded = decode_vector_record(&encoded).unwrap();
        assert_eq!(decoded.dims, 256);
        assert_eq!(decoded.dtype, Dtype::I8);
        assert_eq!(decoded.data, record.data);
    }

    #[test]
    fn test_encode_dimension_mismatch() {
        let record = VectorRecord {
            dims: 128,
            dtype: Dtype::F32,
            data: vec![0x00; 100], // wrong: should be 512
        };
        let err = encode_vector_record(&record).unwrap_err();
        assert!(matches!(err, EdgestoreError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_decode_dimension_mismatch() {
        // 128 dims F32 should have 512 bytes of data
        let mut bytes = vec![0u8; 3 + 100];
        bytes[0] = 0;
        bytes[1] = 128; // dims = 128
        bytes[2] = 0;   // dtype = F32
        let err = decode_vector_record(&bytes).unwrap_err();
        assert!(matches!(err, EdgestoreError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_decode_too_short() {
        let err = decode_vector_record(&[0u8, 1]).unwrap_err();
        assert!(matches!(err, EdgestoreError::CorruptData(_)));
    }

    #[test]
    fn test_encode_endianness() {
        let record = VectorRecord {
            dims: 0x1234,
            dtype: Dtype::F32,
            data: vec![0x00; 0x1234 * 4],
        };
        let encoded = encode_vector_record(&record).unwrap();
        assert_eq!(encoded[0], 0x12);
        assert_eq!(encoded[1], 0x34);
        assert_eq!(encoded[2], 0); // F32
    }
}