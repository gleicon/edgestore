use crate::error::EdgestoreError;
use crate::types::Lsn;
use crate::vector::types::{Dtype, VectorRecord};

/// Trait for vector operations on top of a KV engine.
///
/// Vector records are stored under synthetic namespaces (`__vec__{ns}` per D09)
/// so they are isolated from plain KV data.
pub trait VectorEngine {
    /// Store a vector record under the given namespace and key.
    fn vector_put(
        &mut self,
        ns: &[u8],
        key: &[u8],
        dims: u16,
        dtype: Dtype,
        data: &[u8],
    ) -> Result<Lsn, EdgestoreError>;

    /// Retrieve a vector record by namespace and key.
    fn vector_get(
        &self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Option<VectorRecord>, EdgestoreError>;

    /// Delete a vector record by namespace and key.
    fn vector_delete(
        &mut self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Lsn, EdgestoreError>;
}

/// Generate the synthetic namespace for vector storage.
///
/// Prepends `__vec__` to the user-supplied namespace bytes.
/// Per D09: this isolates vector records from plain KV data.
pub fn vector_namespace(ns: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(7 + ns.len());
    out.extend_from_slice(b"__vec__");
    out.extend_from_slice(ns);
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgestoreConfig, Engine};
    use tempfile::TempDir;

    #[test]
    fn test_vector_namespace() {
        assert_eq!(vector_namespace(b"images"), b"__vec__images");
        assert_eq!(vector_namespace(b""), b"__vec__");
    }

    #[test]
    fn test_vector_put_get_roundtrip_f32() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        let data = vec![0xAB; 128 * 4];
        engine.vector_put(b"ns", b"key1", 128, Dtype::F32, &data).unwrap();

        let record = engine.vector_get(b"ns", b"key1").unwrap().unwrap();
        assert_eq!(record.dims, 128);
        assert_eq!(record.dtype, Dtype::F32);
        assert_eq!(record.data, data);
    }

    #[test]
    fn test_vector_put_dimension_mismatch() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // 128 dims F32 should need 512 bytes, but we pass 100
        let err = engine
            .vector_put(b"ns", b"key1", 128, Dtype::F32, &[0x00; 100])
            .unwrap_err();
        assert!(matches!(err, EdgestoreError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_vector_namespace_isolation() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Store vector under "ns"
        let data = vec![0xAB; 128 * 4];
        engine.vector_put(b"ns", b"key1", 128, Dtype::F32, &data).unwrap();

        // Plain get on "ns" should return None (data is under __vec__ns)
        let plain = engine.get(b"ns", b"key1").unwrap();
        assert_eq!(plain, None);

        // vector_get should find it
        let vec = engine.vector_get(b"ns", b"key1").unwrap();
        assert!(vec.is_some());
    }

    #[test]
    fn test_vector_delete() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        let data = vec![0xAB; 128 * 4];
        engine.vector_put(b"ns", b"key1", 128, Dtype::F32, &data).unwrap();
        assert!(engine.vector_get(b"ns", b"key1").unwrap().is_some());

        engine.vector_delete(b"ns", b"key1").unwrap();
        assert_eq!(engine.vector_get(b"ns", b"key1").unwrap(), None);
    }

    #[test]
    fn test_vector_put_get_roundtrip_f16() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        let data = vec![0xCD; 64 * 2];
        engine.vector_put(b"ns", b"key1", 64, Dtype::F16, &data).unwrap();

        let record = engine.vector_get(b"ns", b"key1").unwrap().unwrap();
        assert_eq!(record.dims, 64);
        assert_eq!(record.dtype, Dtype::F16);
        assert_eq!(record.data, data);
    }

    #[test]
    fn test_vector_put_get_roundtrip_i8() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        let data = vec![0xEF; 256];
        engine.vector_put(b"ns", b"key1", 256, Dtype::I8, &data).unwrap();

        let record = engine.vector_get(b"ns", b"key1").unwrap().unwrap();
        assert_eq!(record.dims, 256);
        assert_eq!(record.dtype, Dtype::I8);
        assert_eq!(record.data, data);
    }
}