//! Envelopes JSONL compartidos por `fleetd` y `fleetctl`.
//!
//! P1 usa TCP en loopback como transporte local portátil; el framing y los tipos
//! son los mismos que se usarían sobre IPC o SSH. No hay TTL de conexión global.

use std::io::{self, BufRead, Write};

use fleet_domain::{Event, OperationId, ProviderId, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

pub const WIRE: &str = "fleet-v1-p1.1";
pub mod transport;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        client_id: String,
        /// Solo se usa donde el filesystem no puede aislar por usuario (Windows
        /// en P1.1); en Unix el socket con permisos 0600 es la garantía.
        token: Option<String>,
    },
    RegisterWorkspace {
        path: String,
    },
    ListWorkspaces,
    CreateSession {
        workspace_id: WorkspaceId,
        provider_id: ProviderId,
    },
    ListSessions,
    SendPrompt {
        session_id: SessionId,
        text: String,
        operation_key: OperationId,
    },
    SubscribeTimeline {
        session_id: SessionId,
        since_seq: u64,
    },
    StopSession {
        session_id: SessionId,
    },
    ListAgents,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        server_id: String,
        wire: String,
    },
    WorkspaceRegistered {
        workspace: WorkspaceSummary,
    },
    Workspaces {
        workspaces: Vec<WorkspaceSummary>,
    },
    SessionCreated {
        session: SessionSummary,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    PromptAccepted {
        operation_id: String,
        session_id: SessionId,
        turn_id: String,
    },
    Subscribed {
        session_id: SessionId,
        latest_seq: u64,
    },
    Stopped {
        session_id: SessionId,
    },
    Agents {
        agents: Vec<AgentSummary>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub provider_id: ProviderId,
    pub state: String,
    pub turn_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: ProviderId,
    /// `None` cuando el proveedor está registrado pero no detectado.
    pub version: Option<String>,
    pub availability: String,
    pub capabilities: fleet_domain::ProviderCapabilities,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Request {
        request_id: String,
        request: Request,
    },
    Response {
        request_id: String,
        result: Result<Response, ApiError>,
    },
    Event {
        session_id: SessionId,
        seq: u64,
        event: Event,
    },
    /// Aviso de transporte/flujo, por ejemplo Backpressure. No consume `seq`.
    Notice {
        session_id: Option<SessionId>,
        code: String,
        message: String,
    },
}

pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let line =
        serde_json::to_string(frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Frame>> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    let frame =
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let frame = Frame::Request {
            request_id: "r1".into(),
            request: Request::Hello {
                client_id: "cli".into(),
                token: None,
            },
        };
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).unwrap();
        let mut reader = io::BufReader::new(buffer.as_slice());
        assert_eq!(read_frame(&mut reader).unwrap(), Some(frame));
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }
}
