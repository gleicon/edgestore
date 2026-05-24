use crate::error::EdgestoreError;
use crate::vector::types::Dtype;

/// Distance metric for vector comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Cosine distance: `1 - dot(q,c) / (|q| * |c|)`.
    /// Range [0, 2] where 0 = identical.
    Cosine,
    /// Euclidean (L2) distance: `sqrt(sum((q_i - c_i)^2))`.
    L2,
    /// Negated dot product: `-dot(q,c)`.
    /// Negated so that lower = better (consistent with min-heap search).
    DotProduct,
}

/// Decode raw bytes to f32 slices based on dtype.
fn decode_to_f32(bytes: &[u8], dtype: Dtype) -> Result<Vec<f32>, EdgestoreError> {
    match dtype {
        Dtype::F32 => {
            if !bytes.len().is_multiple_of(4) {
                return Err(EdgestoreError::CorruptData(
                    "f32 data length not multiple of 4".to_string(),
                ));
            }
            let mut out = Vec::with_capacity(bytes.len() / 4);
            for chunk in bytes.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            Ok(out)
        }
        Dtype::F16 => {
            if !bytes.len().is_multiple_of(2) {
                return Err(EdgestoreError::CorruptData(
                    "f16 data length not multiple of 2".to_string(),
                ));
            }
            let mut out = Vec::with_capacity(bytes.len() / 2);
            for chunk in bytes.chunks_exact(2) {
                let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
                // Manual f16 to f32 conversion using standard library
                out.push(f16_to_f32(bits));
            }
            Ok(out)
        }
        Dtype::I8 => {
            let mut out = Vec::with_capacity(bytes.len());
            for &b in bytes {
                out.push(b as i8 as f32);
            }
            Ok(out)
        }
    }
}

/// Convert f16 bits (u16) to f32.
fn f16_to_f32(bits: u16) -> f32 {
    // Standard IEEE 754 half-precision to single-precision conversion
    let sign = (bits >> 15) & 0x1;
    let exp = ((bits >> 10) & 0x1F) as i32;
    let mant = (bits & 0x3FF) as u32;

    if exp == 0 {
        // Zero or subnormal
        if mant == 0 {
            // Zero
            if sign == 1 { -0.0 } else { 0.0 }
        } else {
            // Subnormal
            let val = (mant as f32) * (2f32.powi(-24));
            if sign == 1 { -val } else { val }
        }
    } else if exp == 31 {
        // Infinity or NaN
        if mant == 0 {
            if sign == 1 { f32::NEG_INFINITY } else { f32::INFINITY }
        } else {
            f32::NAN
        }
    } else {
        // Normal
        let exp = exp - 15 + 127;
        let bits = (sign as u32) << 31 | ((exp as u32) << 23) | (mant << 13);
        f32::from_bits(bits)
    }
}

/// Scalar reference implementation of distance metrics.
///
/// Accepts pre-decoded f32 slices.
pub fn distance_scalar(query: &[f32], candidate: &[f32], metric: Metric) -> f32 {
    assert_eq!(query.len(), candidate.len(), "dimension mismatch");

    match metric {
        Metric::Cosine => {
            let mut dot = 0.0f32;
            let mut norm_q = 0.0f32;
            let mut norm_c = 0.0f32;
            for i in 0..query.len() {
                let q = query[i];
                let c = candidate[i];
                dot += q * c;
                norm_q += q * q;
                norm_c += c * c;
            }
            let denom = norm_q.sqrt() * norm_c.sqrt();
            if denom == 0.0 {
                0.0
            } else {
                1.0 - dot / denom
            }
        }
        Metric::L2 => {
            let mut sum = 0.0f32;
            for i in 0..query.len() {
                let diff = query[i] - candidate[i];
                sum += diff * diff;
            }
            sum.sqrt()
        }
        Metric::DotProduct => {
            let mut dot = 0.0f32;
            for i in 0..query.len() {
                dot += query[i] * candidate[i];
            }
            -dot
        }
    }
}

/// SIMD-accelerated distance for f32 dtype using the `wide` crate.
#[cfg(target_arch = "x86_64")]
pub fn distance_simd_f32(query: &[f32], candidate: &[f32], metric: Metric) -> f32 {
    use wide::f32x8;

    assert_eq!(query.len(), candidate.len(), "dimension mismatch");
    let n = query.len();

    match metric {
        Metric::Cosine => {
            let mut dot_acc = f32x8::ZERO;
            let mut norm_q_acc = f32x8::ZERO;
            let mut norm_c_acc = f32x8::ZERO;

            let chunks = n / 8;
            for i in 0..chunks {
                let offset = i * 8;
                let q = f32x8::from(&query[offset..offset + 8]);
                let c = f32x8::from(&candidate[offset..offset + 8]);
                dot_acc += q * c;
                norm_q_acc += q * q;
                norm_c_acc += c * c;
            }

            let mut dot = dot_acc.reduce_add();
            let mut norm_q = norm_q_acc.reduce_add();
            let mut norm_c = norm_c_acc.reduce_add();

            // Scalar tail
            for i in chunks * 8..n {
                let q = query[i];
                let c = candidate[i];
                dot += q * c;
                norm_q += q * q;
                norm_c += c * c;
            }

            let denom = norm_q.sqrt() * norm_c.sqrt();
            if denom == 0.0 {
                0.0
            } else {
                1.0 - dot / denom
            }
        }
        Metric::L2 => {
            let mut sum_acc = f32x8::ZERO;

            let chunks = n / 8;
            for i in 0..chunks {
                let offset = i * 8;
                let q = f32x8::from(&query[offset..offset + 8]);
                let c = f32x8::from(&candidate[offset..offset + 8]);
                let diff = q - c;
                sum_acc += diff * diff;
            }

            let mut sum = sum_acc.reduce_add();

            // Scalar tail
            for i in chunks * 8..n {
                let diff = query[i] - candidate[i];
                sum += diff * diff;
            }

            sum.sqrt()
        }
        Metric::DotProduct => {
            let mut dot_acc = f32x8::ZERO;

            let chunks = n / 8;
            for i in 0..chunks {
                let offset = i * 8;
                let q = f32x8::from(&query[offset..offset + 8]);
                let c = f32x8::from(&candidate[offset..offset + 8]);
                dot_acc += q * c;
            }

            let mut dot = dot_acc.reduce_add();

            // Scalar tail
            for i in chunks * 8..n {
                dot += query[i] * candidate[i];
            }

            -dot
        }
    }
}

/// SIMD-accelerated distance for f32 on non-x86_64 (fallback to scalar).
#[cfg(not(target_arch = "x86_64"))]
pub fn distance_simd_f32(query: &[f32], candidate: &[f32], metric: Metric) -> f32 {
    distance_scalar(query, candidate, metric)
}

/// Public distance API.
///
/// Decodes raw bytes based on dtype, then dispatches to SIMD (f32) or scalar (f16/i8).
pub fn distance(
    query: &[u8],
    candidate: &[u8],
    dtype: Dtype,
    metric: Metric,
) -> Result<f32, EdgestoreError> {
    if dtype == Dtype::F32 {
        let q = decode_to_f32(query, dtype)?;
        let c = decode_to_f32(candidate, dtype)?;
        if q.len() != c.len() {
            return Err(EdgestoreError::DimensionMismatch {
                expected: q.len(),
                actual: c.len(),
            });
        }
        Ok(distance_simd_f32(&q, &c, metric))
    } else {
        let q = decode_to_f32(query, dtype)?;
        let c = decode_to_f32(candidate, dtype)?;
        if q.len() != c.len() {
            return Err(EdgestoreError::DimensionMismatch {
                expected: q.len(),
                actual: c.len(),
            });
        }
        Ok(distance_scalar(&q, &c, metric))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0f32, 2.0, 3.0];
        let d = distance_scalar(&a, &a, Metric::Cosine);
        assert!((d - 0.0).abs() < 1e-6, "cosine distance to self should be 0, got {}", d);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let d = distance_scalar(&a, &b, Metric::Cosine);
        assert!((d - 1.0).abs() < 1e-6, "cosine distance of orthogonal vectors should be 1, got {}", d);
    }

    #[test]
    fn test_l2_identical() {
        let a = vec![1.0f32, 2.0, 3.0];
        let d = distance_scalar(&a, &a, Metric::L2);
        assert!((d - 0.0).abs() < 1e-6, "L2 distance to self should be 0, got {}", d);
    }

    #[test]
    fn test_l2_known_distance() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![4.0f32, 0.0, 1.0];
        let d = distance_scalar(&a, &b, Metric::L2);
        let expected = ((9.0f32 + 4.0 + 4.0) as f32).sqrt();
        assert!((d - expected).abs() < 1e-5, "L2 distance mismatch: got {}, expected {}", d, expected);
    }

    #[test]
    fn test_dot_product_ordering() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 0.0];
        let c = vec![0.0f32, 1.0];
        let d_ab = distance_scalar(&a, &b, Metric::DotProduct);
        let d_ac = distance_scalar(&a, &c, Metric::DotProduct);
        // a·b = 1, a·c = 0, so -a·b < -a·c → d_ab < d_ac
        assert!(d_ab < d_ac, "dot product ordering: d_ab={} should be < d_ac={}", d_ab, d_ac);
    }

    #[test]
    fn test_simd_scalar_parity_f32() {
        // Deterministic pseudo-random sequence (no external rand dep)
        let dims = 128usize;
        let mut q = Vec::with_capacity(dims);
        let mut c = Vec::with_capacity(dims);
        let mut seed = 12345u64;
        for _ in 0..dims {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            q.push((seed as f32) / (u64::MAX as f32));
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            c.push((seed as f32) / (u64::MAX as f32));
        }

        for metric in [Metric::Cosine, Metric::L2, Metric::DotProduct] {
            let scalar = distance_scalar(&q, &c, metric);
            let simd = distance_simd_f32(&q, &c, metric);
            let diff = (scalar - simd).abs();
            assert!(
                diff < 1e-4,
                "SIMD-scalar parity failed for {:?}: scalar={}, simd={}, diff={}",
                metric,
                scalar,
                simd,
                diff
            );
        }
    }

    #[test]
    fn test_f16_distance() {
        // Two f16 vectors: [1.0, 2.0] and [3.0, 4.0]
        // f16 little-endian bytes: value 0xABCD → [0xCD, 0xAB]
        let a_f16 = vec![
            0x00, 0x3C, // 1.0 in f16 (little-endian: 0x3C00)
            0x00, 0x40, // 2.0 in f16 (little-endian: 0x4000)
        ];
        let b_f16 = vec![
            0x00, 0x42, // 3.0 in f16 (little-endian: 0x4200)
            0x00, 0x44, // 4.0 in f16 (little-endian: 0x4400)
        ];

        let d_l2 = distance(&a_f16, &b_f16, Dtype::F16, Metric::L2).unwrap();
        let expected = ((4.0f32 + 4.0) as f32).sqrt();
        assert!((d_l2 - expected).abs() < 0.1, "f16 L2 mismatch: got {}, expected {}", d_l2, expected);
    }

    #[test]
    fn test_i8_distance() {
        let a_i8 = vec![1i8 as u8, 2i8 as u8];
        let b_i8 = vec![3i8 as u8, 4i8 as u8];

        let d_l2 = distance(&a_i8, &b_i8, Dtype::I8, Metric::L2).unwrap();
        let expected = ((4.0f32 + 4.0) as f32).sqrt();
        assert!((d_l2 - expected).abs() < 1e-5, "i8 L2 mismatch: got {}, expected {}", d_l2, expected);
    }

    #[test]
    fn test_distance_api_f32() {
        let a = vec![1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat();
        let b = vec![3.0f32.to_le_bytes(), 4.0f32.to_le_bytes()].concat();

        let d_l2 = distance(&a, &b, Dtype::F32, Metric::L2).unwrap();
        let expected = ((4.0f32 + 4.0) as f32).sqrt();
        assert!((d_l2 - expected).abs() < 1e-5, "API L2 mismatch: got {}, expected {}", d_l2, expected);
    }
}