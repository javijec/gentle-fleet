//! Test-only read-only probe of an isolated W1-E fixture spool.
use fleet_domain::{AgentEvent, DomainEvent, TurnStatus};
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = std::path::PathBuf::from(args.next().ok_or("missing fixture spool path")?);
    let turn = args
        .next()
        .ok_or("missing fixture turn")?
        .into_string()
        .map_err(|_| "invalid turn")?;
    let key = args
        .next()
        .ok_or("missing fixture operation key")?
        .into_string()
        .map_err(|_| "invalid operation key")?;
    if args.next().is_some() || !path.is_absolute() || !path.is_file() {
        return Err("invalid fixture spool arguments".into());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut statement =
            connection.prepare("SELECT event FROM spool WHERE acked=0 ORDER BY runner_seq")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut terminals = 0;
        for row in rows {
            let event: AgentEvent = serde_json::from_str(&row?)?;
            if matches!(event.kind, DomainEvent::TurnCompleted { turn_id, status: TurnStatus::Completed } if turn_id == turn)
            {
                terminals += 1;
            }
        }
        let operations: i64 = connection.query_row(
            "SELECT COUNT(*) FROM operations WHERE operation_key=?1",
            [&key],
            |row| row.get(0),
        )?;
        if terminals == 1 && operations == 1 {
            println!("fixture terminal is durable and unacknowledged");
            return Ok(());
        }
        if terminals > 1 || Instant::now() >= deadline {
            return Err("offline terminal not observed exactly once".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
