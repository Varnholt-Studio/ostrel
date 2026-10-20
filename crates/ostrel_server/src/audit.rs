//! Audit log of elevated executions (ARCHITECTURE 5.7, AC-32, D117, D118).
//!
//! The VM hands every entry into an elevated function to
//! [`ServerHost::audit`](ostrel_vm::host::ServerHost::audit) as an
//! [`AuditEntry`] that carries the resolved arguments. The server turns it into an
//! [`AuditRecord`] right away: the arguments are replaced by the SHA-256 of the canonical
//! encoding (ARCHITECTURE 5.3) of the argument list as one `List` value, and only that
//! record is stored. The arguments are never stored and never written to a log.

use std::fmt;

use ostrel_core::canon;
use ostrel_core::ids::RowId;
use ostrel_core::value::{SafeInt, Value};
use ostrel_vm::host::{AuditEntry, AuditError, BoxFuture};
use sha2::{Digest, Sha256};

use crate::sha256_hex;

/// SHA-256 of the canonical encoding of `arguments` as one `List` value (D117).
///
/// Equal argument lists give equal hashes; `Set` and `Map` arguments hash the same whatever
/// the order of their elements, because the canonical encoding sorts them (D61).
pub fn hash_arguments(arguments: &[Value]) -> [u8; 32] {
    let list = Value::List(arguments.to_vec());
    Sha256::digest(canon::encode(&list).as_bytes()).into()
}

/// What the server stores for one elevated execution: function, caller, arguments hash and
/// time (ARCHITECTURE 5.7).
#[derive(Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// Qualified name of the elevated function.
    pub function: String,
    /// The principal of the original request; `None` for an anonymous caller.
    pub principal: Option<RowId>,
    /// SHA-256 of the canonical encoding of the argument list, see [`hash_arguments`].
    pub arguments_sha256: [u8; 32],
    /// Milliseconds since the Unix epoch, from the clock of the host.
    pub time: SafeInt,
}

impl AuditRecord {
    /// The record of `entry`. The arguments of `entry` are hashed and not kept.
    pub fn from_entry(entry: &AuditEntry) -> Self {
        AuditRecord {
            function: entry.function.clone(),
            principal: entry.principal,
            arguments_sha256: hash_arguments(&entry.arguments),
            time: entry.time,
        }
    }
}

impl fmt::Debug for AuditRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuditRecord")
            .field("function", &self.function)
            .field("principal", &self.principal.map(RowId::to_hex))
            .field("arguments_sha256", &sha256_hex(&self.arguments_sha256))
            .field("time", &self.time.get())
            .finish()
    }
}

impl fmt::Display for AuditRecord {
    /// One line for the server log: `audit <function> by <user|anonymous> at <ms>
    /// args sha256:<hex>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let caller = self
            .principal
            .map_or_else(|| "anonymous".to_owned(), RowId::to_hex);
        write!(
            f,
            "audit {} by {} at {} args sha256:{}",
            self.function,
            caller,
            self.time.get(),
            sha256_hex(&self.arguments_sha256)
        )
    }
}

/// Where the server keeps audit records.
///
/// ASSUMPTION: the persistent store (a table of the app database, written outside the
/// transaction of the body, D117) comes with the server task that wires the database; until
/// then [`MemoryAuditLog`] is the only implementation.
pub trait AuditStore {
    /// Appends `record`. On `Err` nothing was stored. The error goes back to the VM through
    /// [`ServerHost::audit`](ostrel_vm::host::ServerHost::audit), whose contract is that the
    /// body then does not run, and the task reports the call as `Unavailable` (D134).
    fn append<'a>(&'a mut self, record: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>>;
}

/// Audit records in memory, in the order they were written.
#[derive(Debug, Default)]
pub struct MemoryAuditLog {
    records: Vec<AuditRecord>,
}

impl MemoryAuditLog {
    /// An empty log.
    pub fn new() -> Self {
        MemoryAuditLog::default()
    }

    /// The records written so far.
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }
}

impl AuditStore for MemoryAuditLog {
    fn append<'a>(&'a mut self, record: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>> {
        Box::pin(async move {
            self.records.push(record);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(v: i64) -> SafeInt {
        SafeInt::new(v).unwrap_or_else(|_| unreachable!("constant in range"))
    }

    fn entry(arguments: Vec<Value>) -> AuditEntry {
        AuditEntry {
            function: "admin.reset".to_owned(),
            principal: RowId::from_hex("0000000000000000000000000000002a").ok(),
            arguments,
            time: int(1_700_000_000_000),
        }
    }

    #[test]
    fn hash_is_sha256_of_the_canonical_list() {
        // The canonical encoding of an empty list is `[]`; its SHA-256 is a fixed vector.
        assert_eq!(
            sha256_hex(&hash_arguments(&[])),
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        );
        let args = [Value::Text("a".to_owned()), Value::Int(int(1))];
        let expected: [u8; 32] =
            Sha256::digest(canon::encode(&Value::List(args.to_vec())).as_bytes()).into();
        assert_eq!(hash_arguments(&args), expected);
    }

    #[test]
    fn hash_follows_canonical_order_of_sets() {
        let a = [Value::Set(vec![
            Value::Text("x".to_owned()),
            Value::Text("y".to_owned()),
        ])];
        let b = [Value::Set(vec![
            Value::Text("y".to_owned()),
            Value::Text("x".to_owned()),
        ])];
        assert_eq!(hash_arguments(&a), hash_arguments(&b));
        let list = [Value::List(vec![
            Value::Text("y".to_owned()),
            Value::Text("x".to_owned()),
        ])];
        assert_ne!(hash_arguments(&a), hash_arguments(&list));
    }

    #[test]
    fn record_keeps_no_argument() {
        let secret = "s3cret-argument-value";
        let entry = entry(vec![Value::Text(secret.to_owned())]);
        let record = AuditRecord::from_entry(&entry);
        assert_eq!(record.function, "admin.reset");
        assert_eq!(record.principal, entry.principal);
        assert_eq!(record.time, entry.time);
        assert_eq!(record.arguments_sha256, hash_arguments(&entry.arguments));
        let debug = format!("{record:?}");
        let line = record.to_string();
        assert!(!debug.contains(secret), "{debug}");
        assert!(!line.contains(secret), "{line}");
        assert!(
            line.contains(&sha256_hex(&record.arguments_sha256)),
            "{line}"
        );
        assert!(line.contains("0000000000000000000000000000002a"), "{line}");
    }

    #[test]
    fn anonymous_caller_is_named_in_the_log_line() {
        let mut entry = entry(Vec::new());
        entry.principal = None;
        let line = AuditRecord::from_entry(&entry).to_string();
        assert!(
            line.starts_with("audit admin.reset by anonymous at "),
            "{line}"
        );
    }
}
