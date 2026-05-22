use crate::error::EdgestoreError;
use crate::types::{Lsn, WalRecord, Operation};

#[derive(Debug, PartialEq)]
enum TxState {
    Active,
    Committed,
    RolledBack,
}

pub struct Transaction {
    pub(crate) pending: Vec<WalRecord>,
    pub(crate) txid: u64,
    state: TxState,
}

impl Transaction {
    pub fn new(txid: u64) -> Self {
        Transaction {
            pending: Vec::new(),
            txid,
            state: TxState::Active,
        }
    }

    pub fn put(&mut self, ns: &[u8], key: &[u8], val: &[u8], lsn: Lsn, timestamp: i64) -> Result<(), EdgestoreError> {
        if self.state != TxState::Active {
            return Err(EdgestoreError::InvalidOperation("transaction not active".to_string()));
        }
        let record = WalRecord {
            txid: self.txid,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Put,
            value_hash: blake3::hash(val).into(),
            value_bytes: val.to_vec(),
        };
        self.pending.push(record);
        Ok(())
    }

    pub fn put_with_ttl(&mut self, ns: &[u8], key: &[u8], val: &[u8], ttl_secs: u32, lsn: Lsn, timestamp: i64) -> Result<(), EdgestoreError> {
        if self.state != TxState::Active {
            return Err(EdgestoreError::InvalidOperation("transaction not active".to_string()));
        }
        let record = WalRecord {
            txid: self.txid,
            lsn,
            timestamp,
            ttl: ttl_secs,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Put,
            value_hash: blake3::hash(val).into(),
            value_bytes: val.to_vec(),
        };
        self.pending.push(record);
        Ok(())
    }

    pub fn delete(&mut self, ns: &[u8], key: &[u8], lsn: Lsn, timestamp: i64) -> Result<(), EdgestoreError> {
        if self.state != TxState::Active {
            return Err(EdgestoreError::InvalidOperation("transaction not active".to_string()));
        }
        let record = WalRecord {
            txid: self.txid,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Delete,
            value_hash: blake3::hash(b"").into(),
            value_bytes: vec![],
        };
        self.pending.push(record);
        Ok(())
    }

    pub fn rollback_self(&mut self) {
        self.state = TxState::RolledBack;
        self.pending.clear();
    }

    pub fn take_pending(&mut self) -> Result<Vec<WalRecord>, EdgestoreError> {
        match self.state {
            TxState::Active => {
                self.state = TxState::Committed;
                Ok(std::mem::take(&mut self.pending))
            }
            TxState::Committed => Err(EdgestoreError::InvalidOperation("already committed".to_string())),
            TxState::RolledBack => Err(EdgestoreError::InvalidOperation("already rolled back".to_string())),
        }
    }

    pub fn is_active(&self) -> bool {
        self.state == TxState::Active
    }

    // Convenience wrappers (CORE-06)
    pub fn commit(self, engine: &mut crate::engine::Engine) -> Result<Lsn, EdgestoreError> {
        engine.commit_transaction(self)
    }

    pub fn rollback(self, engine: &mut crate::engine::Engine) {
        engine.rollback_transaction(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_transaction_is_active() {
        let tx = Transaction::new(1);
        assert!(tx.is_active());
    }

    #[test]
    fn test_rollback_clears_pending() {
        let mut tx = Transaction::new(1);
        tx.put(b"ns", b"k", b"v", 1, 0).unwrap();
        assert_eq!(tx.pending.len(), 1);
        tx.rollback_self();
        assert_eq!(tx.pending.len(), 0);
        assert!(!tx.is_active());
        // subsequent put returns Err
        let result = tx.put(b"ns", b"k2", b"v2", 2, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_take_pending_twice_returns_err() {
        let mut tx = Transaction::new(1);
        tx.put(b"ns", b"k", b"v", 1, 0).unwrap();
        let _ = tx.take_pending().unwrap();
        let result = tx.take_pending();
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_appends_delete_record() {
        let mut tx = Transaction::new(42);
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        tx.delete(b"ns", b"k1", 2, 0).unwrap();
        assert_eq!(tx.pending.len(), 2);
        let del = &tx.pending[1];
        assert_eq!(del.op, Operation::Delete);
        assert_eq!(del.key_bytes, b"k1");
        assert!(del.value_bytes.is_empty());
        assert_eq!(del.txid, 42);
    }

    #[test]
    fn test_delete_on_inactive_tx_returns_err() {
        let mut tx = Transaction::new(1);
        tx.rollback_self();
        let result = tx.delete(b"ns", b"k", 1, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_put_with_ttl_sets_ttl_field() {
        let mut tx = Transaction::new(5);
        tx.put_with_ttl(b"ns", b"key", b"val", 3600, 1, 0).unwrap();
        assert_eq!(tx.pending.len(), 1);
        assert_eq!(tx.pending[0].ttl, 3600);
        assert_eq!(tx.pending[0].op, Operation::Put);
        assert_eq!(tx.pending[0].value_bytes, b"val");
    }

    #[test]
    fn test_put_with_ttl_on_inactive_tx_returns_err() {
        let mut tx = Transaction::new(1);
        tx.rollback_self();
        let result = tx.put_with_ttl(b"ns", b"k", b"v", 60, 1, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_take_pending_after_rollback_returns_err() {
        let mut tx = Transaction::new(1);
        tx.put(b"ns", b"k", b"v", 1, 0).unwrap();
        tx.rollback_self();
        let result = tx.take_pending();
        assert!(result.is_err());
    }
}
