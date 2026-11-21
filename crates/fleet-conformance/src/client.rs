//! Cliente RPC acotado y lectura completa de timeline para conformidad P4.4-H.

use std::io::{BufReader, BufWriter};
use std::time::{Duration, Instant};

use fleet_domain::{Event, EventKind};
use fleet_protocol::transport::{connect, expected_token, LocalStream};
use fleet_protocol::{read_frame, write_frame, Frame, Request, Response};

const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const PAGE_SIZE: usize = 256;
const MAX_EVENTS: usize = 65_536;

fn set_timeout(stream: &LocalStream, timeout: Duration) -> Result<(), String> {
    match stream {
        #[cfg(unix)]
        LocalStream::Unix(inner) => {
            inner.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
            inner.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        }
        #[cfg(windows)]
        LocalStream::Tcp(inner) => {
            inner.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
            inner.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub struct ApiClient {
    reader: BufReader<LocalStream>,
    writer: BufWriter<LocalStream>,
    next_id: u64,
}

impl ApiClient {
    pub fn connect() -> Result<Self, String> {
        let stream = connect().map_err(|e| format!("cannot connect to fleetd: {e}"))?;
        set_timeout(&stream, CALL_TIMEOUT)?;
        let mut client = Self {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            writer: BufWriter::new(stream),
            next_id: 0,
        };
        match client.call(Request::Hello {
            client_id: "fleet-conformance".into(),
            token: expected_token().map_err(|e| e.to_string())?,
        })? {
            Response::Hello { .. } => Ok(client),
            other => Err(format!("unexpected Hello response: {other:?}")),
        }
    }

    fn send(&mut self, request: Request) -> Result<String, String> {
        self.next_id += 1;
        let request_id = format!("cf-{}", self.next_id);
        write_frame(
            &mut self.writer,
            &Frame::Request {
                request_id: request_id.clone(),
                request,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(request_id)
    }

    pub fn call(&mut self, request: Request) -> Result<Response, String> {
        let request_id = self.send(request)?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("RPC timeout waiting for {request_id}"));
            }
            match read_frame(&mut self.reader).map_err(|e| e.to_string())? {
                Some(Frame::Response { request_id: id, result }) if id == request_id => {
                    return result.map_err(|e| format!("{}: {}", e.code, e.message));
                }
                Some(Frame::Event { .. }) | Some(Frame::Notice { .. }) => {}
                Some(Frame::Response { .. }) | Some(Frame::Request { .. }) => {
                    return Err("RPC response/request correlation mismatch".into());
                }
                None => return Err("fleetd closed the connection".into()),
            }
        }
    }

    /// Obtiene TODA la timeline por cursor, sin el truncamiento silencioso
    /// de una primera página de 256 eventos.
    pub fn timeline_all(&mut self, session_id: &str) -> Result<Vec<Event>, String> {
        let mut cursor = 0;
        let mut all = Vec::new();
        loop {
            let page = match self.call(Request::ReadTimeline {
                session_id: session_id.into(),
                since_seq: cursor,
                limit: PAGE_SIZE,
            })? {
                Response::Timeline { events, .. } => events,
                other => return Err(format!("unexpected ReadTimeline response: {other:?}")),
            };
            if page.is_empty() {
                break;
            }
            for event in page {
                if event.seq != cursor + 1 {
                    return Err(format!(
                        "timeline gap or duplicate: expected {}, got {}",
                        cursor + 1,
                        event.seq
                    ));
                }
                if event.session_id != session_id {
                    return Err("timeline contains event from a different session".into());
                }
                cursor = event.seq;
                all.push(event);
                if all.len() > MAX_EVENTS {
                    return Err("conformance timeline exceeds explicit event limit".into());
                }
            }
            if all.len() % PAGE_SIZE != 0 {
                break;
            }
        }
        Ok(all)
    }

    /// Suscripción en UNA conexión independiente. Recibe el replay del wire
    /// (Frame::Event) hasta el ACK de snapshot, no dos suscripciones simuladas
    /// sobre el mismo socket.
    pub fn subscribe_snapshot(&mut self, session_id: &str) -> Result<Vec<Event>, String> {
        let id = self.send(Request::SubscribeTimeline {
            session_id: session_id.into(),
            since_seq: 0,
        })?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        let mut backlog = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err("timed out waiting for subscribe ACK".into());
            }
            match read_frame(&mut self.reader).map_err(|e| e.to_string())? {
                Some(Frame::Event { session_id: sid, seq, event }) => {
                    if sid != session_id || event.session_id != session_id || seq != event.seq {
                        return Err("subscribe replay returned an unrelated event".into());
                    }
                    if seq != backlog.len() as u64 + 1 {
                        return Err(format!("subscribe replay gap at {seq}"));
                    }
                    backlog.push(event);
                    if backlog.len() > MAX_EVENTS {
                        return Err("subscriber backlog exceeded maximum size".into());
                    }
                }
                Some(Frame::Response { request_id, result }) if request_id == id => {
                    match result.map_err(|e| format!("{}: {}", e.code, e.message))? {
                        Response::Subscribed { latest_seq, .. }
                            if latest_seq == backlog.len() as u64 => return Ok(backlog),
                        other => return Err(format!("subscribe ACK/backlog mismatch: {other:?}")),
                    }
                }
                Some(Frame::Notice { code, message, .. }) => {
                    return Err(format!("subscribe notice {code}: {message}"));
                }
                Some(_) => return Err("unexpected frame during subscriber replay".into()),
                None => return Err("fleetd disconnected subscriber".into()),
            }
        }
    }

    /// La condición terminal debe pertenecer al turno exacto bajo prueba.
    /// La consulta no reescanea siempre solo los 256 primeros eventos.
    pub fn wait_for_terminal(
        &mut self,
        session_id: &str,
        turn_id: &str,
        timeout: Duration,
    ) -> Result<Vec<Event>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let events = self.timeline_all(session_id)?;
            if events.iter().any(|event| match &event.kind {
                EventKind::TurnCompleted { turn_id: id, .. }
                | EventKind::TurnUncertain { turn_id: id, .. } => id == turn_id,
                _ => false,
            }) {
                return Ok(events);
            }
            if Instant::now() >= deadline {
                return Err("turn did not reach a terminal event before the deadline".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
