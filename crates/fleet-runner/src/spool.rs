//! Spool durable y acotado del runner. No es la base autoritativa de Fleet:
//! sólo conserva eventos no confirmados por el daemon.

use std::path::Path;
use std::sync::Mutex;

use fleet_domain::AgentEvent;
use fleet_protocol::runner::{MAX_SPOOL_BYTES, MAX_SPOOL_EVENTS};
use rusqlite::{params, Connection, TransactionBehavior};

pub struct Spool {
    connection: Mutex<Connection>,
}

impl Spool {
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS spool (
                    runner_seq INTEGER PRIMARY KEY,
                    event TEXT NOT NULL,
                    bytes INTEGER NOT NULL,
                    acked INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS operations (
                    operation_key TEXT PRIMARY KEY,
                    operation_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                 );
                 /* Backpressure evidence survives a full spool; bounded. */
                 CREATE TABLE IF NOT EXISTS notices (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    message TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                 );",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Registra evidencia durable de backpressure con capacidad propia: el
    /// spool principal puede estar lleno, pero la saturación nunca se pierde
    /// silenciosamente. Conserva como máximo las 16 últimas notas.
    pub fn record_notice(&self, message: &str) -> Result<(), String> {
        let mut connection = self.connection.lock().expect("spool poisoned");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO notices(message, created_at) VALUES(?1, ?2)",
            params![message, now_ms() as i64],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM notices WHERE id IN
             (SELECT id FROM notices ORDER BY id DESC LIMIT -1 OFFSET 16)",
            [],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub fn notices(&self) -> Result<Vec<String>, String> {
        let connection = self.connection.lock().expect("spool poisoned");
        let mut statement = connection
            .prepare("SELECT message FROM notices ORDER BY id DESC LIMIT 16")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn unacked_count(&self) -> Result<(usize, usize), String> {
        let connection = self.connection.lock().expect("spool poisoned");
        let (count, bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM spool WHERE acked=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        Ok((count as usize, bytes as usize))
    }

    fn next_seq_locked(&self, connection: &Connection) -> Result<u64, String> {
        let seq: i64 = connection
            .query_row("SELECT COALESCE(MAX(runner_seq),0) FROM spool", [], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        Ok(seq as u64 + 1)
    }

    /// Persiste antes de devolver. Devuelve `spool_full` sin descartar eventos:
    /// el llamado debe reducir la presión antes de seguir produciendo.
    pub fn append(&self, event: &AgentEvent) -> Result<u64, String> {
        let mut connection = self.connection.lock().expect("spool poisoned");
        let (count, bytes) = self.unacked_count_locked(&connection)?;
        let payload = serde_json::to_string(event).map_err(|error| error.to_string())?;
        if count + 1 > MAX_SPOOL_EVENTS || bytes + payload.len() > MAX_SPOOL_BYTES {
            return Err("spool_full".into());
        }
        let seq = self.next_seq_locked(&connection)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO spool(runner_seq,event,bytes) VALUES(?1,?2,?3)",
            params![seq as i64, payload, payload.len() as i64],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(seq)
    }

    pub fn pending(&self, since_seq: u64, limit: usize) -> Result<Vec<(u64, AgentEvent)>, String> {
        let connection = self.connection.lock().expect("spool poisoned");
        let mut statement = connection
            .prepare(
                "SELECT runner_seq,event FROM spool WHERE acked=0 AND runner_seq>?1 ORDER BY runner_seq LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![since_seq as i64, limit as i64], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        let mut result = Vec::new();
        for row in rows {
            let (seq, payload) = row.map_err(|error| error.to_string())?;
            let event: AgentEvent =
                serde_json::from_str(&payload).map_err(|error| error.to_string())?;
            result.push((seq, event));
        }
        Ok(result)
    }

    pub fn ack(&self, runner_seq: u64) -> Result<(), String> {
        let mut connection = self.connection.lock().expect("spool poisoned");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE spool SET acked=1 WHERE runner_seq<=?1 AND acked=0",
            params![runner_seq as i64],
        )
        .map_err(|error| error.to_string())?;
        // Compacta sólo eventos confirmados: nunca elimina evidencia pendiente.
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM spool", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if count > MAX_SPOOL_EVENTS as i64 {
            tx.execute(
                "DELETE FROM spool WHERE acked=1 AND runner_seq IN
                 (SELECT runner_seq FROM spool WHERE acked=1 ORDER BY runner_seq LIMIT ?1)",
                params![count - MAX_SPOOL_EVENTS as i64],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(())
    }

    /// `true` si la operación ya fue aceptada por este runner.
    pub fn reserve_operation(
        &self,
        operation_key: &str,
        operation_id: &str,
    ) -> Result<bool, String> {
        let connection = self.connection.lock().expect("spool poisoned");
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO operations(operation_key,operation_id,created_at) VALUES(?1,?2,?3)",
                params![operation_key, operation_id, now_ms() as i64],
            )
            .map_err(|error| error.to_string())?;
        Ok(changed == 0)
    }

    pub fn last_ack(&self) -> Result<u64, String> {
        let connection = self.connection.lock().expect("spool poisoned");
        let seq: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(runner_seq),0) FROM spool WHERE acked=1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(seq as u64)
    }

    pub fn pending_count(&self) -> Result<usize, String> {
        Ok(self.unacked_count()?.0)
    }

    fn unacked_count_locked(&self, connection: &Connection) -> Result<(usize, usize), String> {
        let (count, bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM spool WHERE acked=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        Ok((count as usize, bytes as usize))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_domain::{AgentEvent, DomainEvent};
    use std::path::PathBuf;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gf-p3-spool-{}-{name}.sqlite", std::process::id()))
    }

    fn event(seq: u64) -> AgentEvent {
        AgentEvent {
            kind: DomainEvent::AssistantDelta {
                turn_id: format!("turn-{seq}"),
                text: "payload".into(),
            },
        }
    }

    #[test]
    fn saturation_never_loses_events_or_hides_backpressure() {
        let db = path("saturation");
        let _ = std::fs::remove_file(&db);
        let spool = Spool::open(&db).unwrap();
        // Simula daemon caído: nadie hace ACK mientras se produce.
        for seq in 1..=MAX_SPOOL_EVENTS {
            assert_eq!(spool.append(&event(seq as u64)).unwrap(), seq as u64);
        }
        assert_eq!(spool.pending_count().unwrap(), MAX_SPOOL_EVENTS);
        let rejected = spool.append(&event(9999));
        assert_eq!(rejected.unwrap_err(), "spool_full");
        assert_eq!(spool.pending_count().unwrap(), MAX_SPOOL_EVENTS);

        // La evidencia de saturación se persiste con capacidad propia.
        spool.record_notice("runner spool full").unwrap();
        assert_eq!(
            spool.notices().unwrap(),
            vec!["runner spool full".to_string()]
        );

        // Ningún evento sin ACK se elimina al compactar.
        spool.ack(1).unwrap();
        assert_eq!(spool.pending_count().unwrap(), MAX_SPOOL_EVENTS - 1);
        assert_eq!(spool.last_ack().unwrap(), 1);
        let pending = spool.pending(1, MAX_SPOOL_EVENTS).unwrap();
        assert_eq!(pending.len(), MAX_SPOOL_EVENTS - 1);
        assert_eq!(pending.first().unwrap().0, 2);
        assert_eq!(pending.last().unwrap().0, MAX_SPOOL_EVENTS as u64);

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn byte_limit_is_enforced_without_partial_writes() {
        let db = path("bytes");
        let _ = std::fs::remove_file(&db);
        let spool = Spool::open(&db).unwrap();
        let big = AgentEvent {
            kind: DomainEvent::AssistantDelta {
                turn_id: "turn-big".into(),
                text: "x".repeat(MAX_SPOOL_BYTES),
            },
        };
        assert_eq!(spool.append(&big).unwrap_err(), "spool_full");
        assert_eq!(spool.pending_count().unwrap(), 0);
        assert_eq!(spool.last_ack().unwrap(), 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn operation_reservation_is_idempotent() {
        let db = path("operations");
        let _ = std::fs::remove_file(&db);
        let spool = Spool::open(&db).unwrap();
        assert!(!spool.reserve_operation("key-1", "op-1").unwrap());
        assert!(spool.reserve_operation("key-1", "op-1").unwrap());
        assert!(!spool.reserve_operation("key-2", "op-2").unwrap());
        let _ = std::fs::remove_file(&db);
    }
}
