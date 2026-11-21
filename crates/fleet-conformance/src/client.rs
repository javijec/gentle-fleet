//! Cliente mínimo de la API Fleet para el framework de conformidad.

use std::io::{BufReader, BufWriter};
use std::time::Duration;

use fleet_protocol::transport::{connect, expected_token, LocalStream};
use fleet_protocol::{read_frame, write_frame, Frame, Request, Response};

pub struct ApiClient {
    reader: BufReader<LocalStream>,
    writer: BufWriter<LocalStream>,
    next_id: u64,
}

impl ApiClient {
    pub fn connect() -> Result<Self, String> {
        let stream = connect().map_err(|e| format!("cannot connect to fleetd: {e}"))?;
        let mut client = Self {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            writer: BufWriter::new(stream),
            next_id: 0,
        };
        client.call(Request::Hello {
            client_id: "fleet-conformance".into(),
            token: expected_token().ok().flatten(),
        })?;
        Ok(client)
    }

    pub fn call(&mut self, request: Request) -> Result<Response, String> {
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
        loop {
            match read_frame(&mut self.reader).map_err(|e| e.to_string())? {
                Some(Frame::Response {
                    request_id: id,
                    result,
                }) if id == request_id => {
                    return result.map_err(|e| format!("{}: {}", e.code, e.message));
                }
                Some(Frame::Event { .. }) | Some(Frame::Notice { .. }) => continue,
                Some(Frame::Response { .. }) | Some(Frame::Request { .. }) => continue,
                None => return Err("fleetd closed the connection".into()),
            }
        }
    }

    /// Lee eventos hasta que aparece un evento del tipo indicado o vence el plazo.
    pub fn wait_for_terminal(
        &mut self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<Vec<fleet_domain::Event>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let events = match self.call(Request::ReadTimeline {
                session_id: session_id.into(),
                since_seq: 0,
                limit: 256,
            })? {
                Response::Timeline { events, .. } => events,
                _ => return Err("unexpected response to ReadTimeline".into()),
            };
            if events.iter().any(|event| {
                matches!(
                    event.kind,
                    fleet_domain::EventKind::TurnCompleted { .. }
                        | fleet_domain::EventKind::TurnUncertain { .. }
                )
            }) {
                return Ok(events);
            }
            if std::time::Instant::now() >= deadline {
                return Err("turn did not reach a terminal event before the deadline".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
