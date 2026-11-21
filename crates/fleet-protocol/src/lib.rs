//! Envelopes JSONL compartidos por `fleetd` y `fleetctl`.
//!
//! P1 usa TCP en loopback como transporte local portátil; el framing y los tipos
//! son los mismos que se usarían sobre IPC o SSH. No hay TTL de conexión global.

use std::io::{self, BufRead, Write};

use fleet_domain::{Event, OperationId, ProviderId, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

pub const WIRE: &str = "fleet-v1-p2";
pub mod runner;
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
    InterruptTurn {
        session_id: SessionId,
        turn_id: String,
    },
    ListControls {
        session_id: SessionId,
    },
    SetControl {
        session_id: SessionId,
        name: String,
        value: String,
        scope: String,
    },
    ListNativeSessions {
        provider_id: ProviderId,
    },
    ImportNativeHistory {
        session_id: SessionId,
        provider_id: ProviderId,
        native_ref: String,
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
    ReadTimeline {
        session_id: SessionId,
        since_seq: u64,
        limit: usize,
    },
    GetOperation {
        operation_key: String,
    },
    ListApprovals {
        session_id: Option<SessionId>,
    },
    GetApproval {
        approval_id: String,
    },
    AnswerApproval {
        approval_id: String,
        decision: String,
        operation_key: String,
        #[serde(default)]
        arguments: Vec<fleet_domain::ApprovalArgument>,
    },
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
    TurnInterrupted {
        session_id: SessionId,
        turn_id: String,
    },
    Controls {
        controls: Vec<fleet_domain::ControlState>,
    },
    ControlChanged {
        control: fleet_domain::ControlState,
    },
    NativeSessions {
        sessions: Vec<fleet_domain::NativeSessionInfo>,
    },
    HistoryImported {
        count: usize,
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
    Timeline {
        session_id: SessionId,
        events: Vec<Event>,
    },
    Operation {
        operation: fleet_domain::store::OperationRecord,
    },
    Approvals {
        approvals: Vec<fleet_domain::store::ApprovalRequest>,
    },
    Approval {
        approval: fleet_domain::store::ApprovalRequest,
    },
    ApprovalResolved {
        approval: fleet_domain::store::ApprovalRequest,
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
    #[serde(default)]
    pub controls: Vec<fleet_domain::ControlState>,
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
    fn approval_wire_never_leaks_native_request_id() {
        let approval = fleet_domain::store::ApprovalRequest {
            approval_id: "appr-1".into(),
            session_id: "ses-1".into(),
            turn_id: "turn-1".into(),
            kind: "shell".into(),
            message: "test".into(),
            resources: vec![],
            allowed_decisions: vec!["once".into()],
            status: fleet_domain::store::ApprovalStatus::Pending,
            native_request_id: "native-sensitive-secret".into(),
            decision: None,
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        let frame = Frame::Response {
            request_id: "req-1".into(),
            result: Ok(Response::Approval { approval }),
        };
        let text = serde_json::to_string(&frame).unwrap();
        assert!(!text.contains("native_request_id"));
        assert!(!text.contains("native-sensitive-secret"));
    }

    #[test]
    fn legacy_client_frames_still_deserialize_after_interrupt_was_added() {
        // P4.1-A.4: ampliar el enum con una variante nueva no debe romper
        // clientes antiguos. Este JSON es el emitido por fleetctl antes de P4.1.
        let legacy = r#"{"kind":"request","request_id":"r-old","request":{"type":"send_prompt","session_id":"ses-1","text":"hola","operation_key":"op-1"}}"#;
        let frame: Frame = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            frame,
            Frame::Request {
                request: Request::SendPrompt { .. },
                ..
            }
        ));
        let interrupt = Frame::Request {
            request_id: "r-new".into(),
            request: Request::InterruptTurn {
                session_id: "ses-1".into(),
                turn_id: "turn-1".into(),
            },
        };
        let text = serde_json::to_string(&interrupt).unwrap();
        assert_eq!(serde_json::from_str::<Frame>(&text).unwrap(), interrupt);
        let legacy_agent = r#"{"id":"fake","version":"0.1.0","availability":"Available","capabilities":{"streaming_text":true,"tool_events":true,"session_history":true,"native_turn_ids":true,"cancel_turn":false,"approvals":false,"can_own_process":false,"can_attach_service":false},"mode":"Owned"}"#;
        let summary: AgentSummary = serde_json::from_str(legacy_agent).unwrap();
        assert!(summary.controls.is_empty());
        assert_eq!(WIRE, "fleet-v1-p2");
    }

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
