//! Protocolo interno daemon ↔ runner. Versionado y tipado; nunca contiene
//! JSON/RPC privado del proveedor ni envelopes del cliente `fleetctl`.

use std::io::{self, BufRead, Write};

use fleet_domain::{AgentEvent, RunnerIdentity};
use serde::{Deserialize, Serialize};

pub fn write_runner_frame<W: Write>(writer: &mut W, frame: &RunnerFrame) -> io::Result<()> {
    let line = serde_json::to_string(frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn read_runner_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<RunnerFrame>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let frame = serde_json::from_str(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(frame))
}

pub const RUNNER_WIRE: &str = "fleet-runner-v1";
pub const MAX_SPOOL_EVENTS: usize = 1024;
pub const MAX_SPOOL_BYTES: usize = 4 * 1024 * 1024;
pub const ACK_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerCommand {
    Prompt {
        text: String,
        turn_id: String,
        operation_key: String,
    },
    Answer {
        approval_id: String,
        native_request_id: String,
        decision: String,
        operation_key: String,
    },
    Interrupt {
        turn_id: String,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunnerFrame {
    Request {
        request_id: String,
        request: RunnerRequest,
    },
    Response {
        request_id: String,
        result: Result<RunnerResponse, RunnerError>,
    },
    Event {
        runner_seq: u64,
        event: AgentEvent,
    },
    Notice {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerRequest {
    Hello {
        identity: RunnerIdentity,
        token: String,
    },
    Status,
    Submit {
        operation_id: String,
        operation_key: String,
        command: RunnerCommand,
    },
    Ack {
        runner_seq: u64,
    },
    Pending {
        since_seq: u64,
        limit: usize,
    },
    QueryApprovals,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerResponse {
    Hello {
        identity: RunnerIdentity,
        status: String,
        last_ack_seq: u64,
        pending_count: usize,
        protocol_version: String,
    },
    Status {
        status: String,
        last_ack_seq: u64,
        pending_count: usize,
        provider_alive: bool,
        /// Notas durables del runner (p.ej. backpressure del spool).
        notices: Vec<String>,
    },
    Accepted {
        operation_id: String,
    },
    Pending {
        events: Vec<(u64, AgentEvent)>,
    },
    Approvals {
        pending: Vec<String>,
    },
    Shutdown {
        status: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerError {
    pub code: String,
    pub message: String,
}

impl RunnerError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_frame_roundtrip_and_wire_constant() {
        assert_eq!(RUNNER_WIRE, "fleet-runner-v1");
        let frame = RunnerFrame::Request {
            request_id: "r1".into(),
            request: RunnerRequest::Status,
        };
        let mut buffer = Vec::new();
        write_runner_frame(&mut buffer, &frame).unwrap();
        let mut reader = std::io::BufReader::new(buffer.as_slice());
        assert_eq!(read_runner_frame(&mut reader).unwrap(), Some(frame));
        assert_eq!(read_runner_frame(&mut reader).unwrap(), None);
    }
}
