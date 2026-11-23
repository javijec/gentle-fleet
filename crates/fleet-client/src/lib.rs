//! Reusable, UI-independent Rust client for the public Fleet API.
//!
//! A client owns exactly one host context and one request/response stream.
//! No daemon state, provider protocol, database, or GUI runtime is exposed.

use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use fleet_domain::{store, ApprovalArgument, ControlState, Event, OperationId, SessionId};
use fleet_protocol::transport::{connect as connect_local, expected_token};
use fleet_protocol::{read_frame, write_frame, AgentSummary, Frame, Request, Response};
use fleet_ssh::{find_host, FleetReader, FleetWriter, SshStream};
use uuid::Uuid;

/// Explicit target. A connection never inherits a process-global host choice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    Local,
    Ssh(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    pub host: Host,
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    Transport(String),
    Protocol(String),
    State(String),
    Api { code: String, message: String },
    UnexpectedResponse(&'static str),
    RetentionGap { expected: u64, received: u64 },
    SubscriptionClosed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) | Self::Protocol(message) | Self::State(message) => {
                f.write_str(message)
            }
            Self::Api { code, message } => write!(f, "{code}: {message}"),
            Self::UnexpectedResponse(expected) => {
                write!(f, "unexpected response; expected {expected}")
            }
            Self::RetentionGap { expected, received } => {
                write!(
                    f,
                    "timeline gap: expected seq {expected}, received {received}"
                )
            }
            Self::SubscriptionClosed => f.write_str("timeline subscription closed"),
        }
    }
}

impl std::error::Error for ClientError {}

pub type Result<T> = std::result::Result<T, ClientError>;

pub struct FleetClient {
    host: Host,
    reader: Option<BufReader<FleetReader>>,
    writer: BufWriter<FleetWriter>,
    next_request_id: u64,
}

impl FleetClient {
    pub fn connect(host: Host) -> Result<Self> {
        let (reader, writer, remote) = match &host {
            Host::Local => {
                let stream =
                    connect_local().map_err(|error| ClientError::Transport(error.to_string()))?;
                let reader = stream
                    .try_clone()
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
                (
                    FleetReader::Local(reader),
                    FleetWriter::Local(stream),
                    false,
                )
            }
            Host::Ssh(alias) => {
                let config =
                    find_host(alias).map_err(|error| ClientError::Transport(error.to_string()))?;
                let ssh = SshStream::connect(&config)
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
                let (reader, writer) = ssh
                    .split()
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
                (reader, writer, true)
            }
        };
        let mut client = Self {
            host,
            reader: Some(BufReader::new(reader)),
            writer: BufWriter::new(writer),
            next_request_id: 0,
        };
        client.hello(remote)?;
        Ok(client)
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn disconnect(&mut self) {
        self.writer.get_mut().close();
    }

    fn hello(&mut self, remote: bool) -> Result<()> {
        match self.request(Request::Hello {
            client_id: "fleet-client".into(),
            token: if remote {
                None
            } else {
                expected_token().ok().flatten()
            },
        })? {
            Response::Hello { wire, .. } if wire == fleet_protocol::WIRE => Ok(()),
            Response::Hello { wire, .. } => Err(ClientError::Protocol(format!(
                "incompatible Fleet wire: expected {}, received {wire}",
                fleet_protocol::WIRE
            ))),
            _ => Err(ClientError::UnexpectedResponse("Hello")),
        }
    }

    /// Low-level typed request entry point shared with fleetctl. Request IDs
    /// are generated independently from idempotency keys in mutating requests.
    pub fn request(&mut self, request: Request) -> Result<Response> {
        self.journal_if_remote(&request)?;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let request_id = format!("sdk-{}", self.next_request_id);
        write_frame(
            &mut self.writer,
            &Frame::Request {
                request_id: request_id.clone(),
                request,
            },
        )
        .map_err(|error| ClientError::Transport(error.to_string()))?;
        loop {
            let frame = read_frame(self.reader.as_mut().expect("client reader present"))
                .map_err(|error| ClientError::Transport(error.to_string()))?;
            let Some(frame) = frame else {
                return Err(ClientError::Transport(self.classify_disconnect()));
            };
            match frame {
                Frame::Response {
                    request_id: id,
                    result,
                } if id == request_id => {
                    return result.map_err(|error| ClientError::Api {
                        code: error.code,
                        message: error.message,
                    });
                }
                Frame::Event { .. } => continue,
                Frame::Notice { code, message, .. } => {
                    return Err(ClientError::Api { code, message });
                }
                _ => continue,
            }
        }
    }

    fn classify_disconnect(&mut self) -> String {
        if !matches!(self.host, Host::Ssh(_)) {
            return "Fleet connection closed".into();
        }
        let code = self.writer.get_mut().ssh_exit_code();
        let diagnostics = self.writer.get_ref().ssh_diagnostics().join(" | ");
        let lower = diagnostics.to_ascii_lowercase();
        if lower.contains("host key verification failed")
            || lower.contains("remote host identification has changed")
            || lower.contains("no matching host key")
        {
            "HostKeyRejected: SSH server identity verification failed".into()
        } else if lower.contains("permission denied (publickey")
            || lower.contains("permission denied (")
        {
            "AuthFailed: SSH authentication rejected".into()
        } else if lower.contains("fleetd unavailable") {
            "DaemonDown: remote fleetd is unavailable".into()
        } else if code == Some(127)
            || lower.contains("fleet-ssh-bridge: not found")
            || lower.contains("fleet-ssh-bridge: no such file")
        {
            "BridgeMissing: remote bridge executable not found".into()
        } else if lower.contains("connection refused") {
            "SSHUnreachable: connection refused".into()
        } else if lower.contains("connection timed out") || lower.contains("operation timed out") {
            "SSHTimeout: connection timed out".into()
        } else if lower.contains("could not resolve hostname") {
            "SSHUnreachable: name resolution failed".into()
        } else {
            // SSH diagnostics may contain local paths, account names or
            // unexpected sensitive data; classify without echoing raw stderr.
            format!("SSHDisconnected: SSH exited with status {code:?}")
        }
    }

    fn journal_if_remote(&self, request: &Request) -> Result<()> {
        let Host::Ssh(alias) = &self.host else {
            return Ok(());
        };
        let (kind, target, key) = match request {
            Request::SendPrompt {
                session_id,
                operation_key,
                ..
            } => ("prompt", session_id.as_str(), operation_key.as_str()),
            Request::AnswerApproval {
                approval_id,
                operation_key,
                ..
            } => ("answer", approval_id.as_str(), operation_key.as_str()),
            Request::ImportNativeHistory {
                session_id,
                operation_key,
                ..
            } => (
                "import-history",
                session_id.as_str(),
                operation_key.as_str(),
            ),
            _ => return Ok(()),
        };
        let path = operation_journal_path()?;
        let parent = path.parent().ok_or_else(|| {
            ClientError::Transport("operation journal needs a parent directory".into())
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| ClientError::Transport(format!("operation journal: {error}")))?;
        #[cfg(unix)]
        if std::env::var_os("FLEET_OPERATION_JOURNAL").is_none() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ClientError::Transport(format!("operation journal: {error}")))?;
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(ClientError::Transport(
                    "operation journal is not a regular file".into(),
                ));
            }
        }
        let mut line = serde_json::to_vec(&serde_json::json!({
            "host": alias,
            "kind": kind,
            "target": target,
            "operation_key": key,
            "created_at_unix_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis(),
        }))
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|error| ClientError::Transport(format!("operation journal: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file
                .metadata()
                .map_err(|error| ClientError::Transport(error.to_string()))?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(ClientError::Transport(
                    "operation journal permissions are too permissive".into(),
                ));
            }
        }
        file.write_all(&line)
            .and_then(|_| file.sync_data())
            .map_err(|error| ClientError::Transport(format!("operation journal: {error}")))
    }

    pub fn new_operation_key(&self) -> OperationId {
        Uuid::new_v4().to_string()
    }

    pub fn list_agents(&mut self) -> Result<Vec<AgentSummary>> {
        match self.request(Request::ListAgents)? {
            Response::Agents { agents } => Ok(agents),
            _ => Err(ClientError::UnexpectedResponse("Agents")),
        }
    }

    pub fn register_workspace(&mut self, path: String) -> Result<fleet_protocol::WorkspaceSummary> {
        match self.request(Request::RegisterWorkspace { path })? {
            Response::WorkspaceRegistered { workspace } => Ok(workspace),
            _ => Err(ClientError::UnexpectedResponse("WorkspaceRegistered")),
        }
    }

    pub fn list_workspaces(&mut self) -> Result<Vec<fleet_protocol::WorkspaceSummary>> {
        match self.request(Request::ListWorkspaces)? {
            Response::Workspaces { workspaces } => Ok(workspaces),
            _ => Err(ClientError::UnexpectedResponse("Workspaces")),
        }
    }

    pub fn create_session(
        &mut self,
        workspace_id: String,
        provider_id: String,
    ) -> Result<fleet_protocol::SessionSummary> {
        match self.request(Request::CreateSession {
            workspace_id,
            provider_id,
        })? {
            Response::SessionCreated { session } => Ok(session),
            _ => Err(ClientError::UnexpectedResponse("SessionCreated")),
        }
    }

    pub fn get_session(
        &mut self,
        session_id: &str,
    ) -> Result<Option<fleet_protocol::SessionSummary>> {
        Ok(self
            .list_sessions()?
            .into_iter()
            .find(|session| session.id == session_id))
    }

    pub fn list_sessions(&mut self) -> Result<Vec<fleet_protocol::SessionSummary>> {
        match self.request(Request::ListSessions)? {
            Response::Sessions { sessions } => Ok(sessions),
            _ => Err(ClientError::UnexpectedResponse("Sessions")),
        }
    }

    pub fn send_prompt(
        &mut self,
        session_id: SessionId,
        text: String,
        operation_key: OperationId,
    ) -> Result<(String, SessionId, String)> {
        match self.request(Request::SendPrompt {
            session_id,
            text,
            operation_key,
        })? {
            Response::PromptAccepted {
                operation_id,
                session_id,
                turn_id,
            } => Ok((operation_id, session_id, turn_id)),
            _ => Err(ClientError::UnexpectedResponse("PromptAccepted")),
        }
    }

    pub fn read_timeline_page(
        &mut self,
        session_id: SessionId,
        since_seq: u64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        match self.request(Request::ReadTimeline {
            session_id,
            since_seq,
            limit: limit.clamp(1, 256),
        })? {
            Response::Timeline { events, .. } => Ok(events),
            _ => Err(ClientError::UnexpectedResponse("Timeline")),
        }
    }

    pub fn read_timeline(&mut self, session_id: SessionId, since_seq: u64) -> Result<Vec<Event>> {
        let mut cursor = since_seq;
        let mut all = Vec::new();
        loop {
            let page = self.read_timeline_page(session_id.clone(), cursor, 256)?;
            if page.is_empty() {
                break;
            }
            for event in page {
                if event.seq != cursor + 1 {
                    return Err(ClientError::RetentionGap {
                        expected: cursor + 1,
                        received: event.seq,
                    });
                }
                cursor = event.seq;
                all.push(event);
            }
            if all.len() > 1_000_000 {
                return Err(ClientError::Protocol(
                    "timeline exceeds SDK safety limit; use read_timeline_page".into(),
                ));
            }
            if !all.is_empty() && all.len() % 256 != 0 {
                break;
            }
        }
        Ok(all)
    }

    pub fn list_native_sessions(
        &mut self,
        provider_id: String,
    ) -> Result<Vec<fleet_domain::NativeSessionInfo>> {
        match self.request(Request::ListNativeSessions { provider_id })? {
            Response::NativeSessions { sessions } => Ok(sessions),
            _ => Err(ClientError::UnexpectedResponse("NativeSessions")),
        }
    }

    pub fn import_native_history(
        &mut self,
        session_id: SessionId,
        provider_id: String,
        native_ref: String,
        operation_key: OperationId,
    ) -> Result<usize> {
        match self.request(Request::ImportNativeHistory {
            session_id,
            provider_id,
            native_ref,
            operation_key,
        })? {
            Response::HistoryImported { count } => Ok(count),
            _ => Err(ClientError::UnexpectedResponse("HistoryImported")),
        }
    }

    pub fn subscribe_timeline(
        &self,
        session_id: SessionId,
        since_seq: u64,
    ) -> Result<TimelineSubscription> {
        let host = self.host.clone();
        let subscribed_session = session_id.clone();
        let cursor = Arc::new(std::sync::atomic::AtomicU64::new(since_seq));
        let (sender, receiver) = mpsc::sync_channel(64);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled = Arc::clone(&cancelled);
        let worker = thread::Builder::new()
            .name("fleet-timeline-subscription".into())
            .spawn(move || {
                subscription_worker(
                    host,
                    subscribed_session,
                    since_seq,
                    sender,
                    thread_cancelled,
                )
            })
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(TimelineSubscription {
            host: self.host.clone(),
            session_id,
            receiver,
            cancelled,
            worker: Some(worker),
            cursor,
        })
    }

    /// Resume observation from the last event delivered by this SDK for this
    /// host/session identity.
    pub fn resume_timeline(&self, session_id: SessionId) -> Result<TimelineSubscription> {
        let since = load_saved_cursor(&self.host, &session_id)?;
        self.subscribe_timeline(session_id, since)
    }

    pub fn get_operation(&mut self, operation_key: String) -> Result<store::OperationRecord> {
        match self.request(Request::GetOperation { operation_key })? {
            Response::Operation { operation } => Ok(operation),
            _ => Err(ClientError::UnexpectedResponse("Operation")),
        }
    }

    pub fn list_approvals(
        &mut self,
        session_id: Option<SessionId>,
    ) -> Result<Vec<store::ApprovalRequest>> {
        match self.request(Request::ListApprovals { session_id })? {
            Response::Approvals { approvals } => Ok(approvals),
            _ => Err(ClientError::UnexpectedResponse("Approvals")),
        }
    }

    pub fn get_approval(&mut self, approval_id: String) -> Result<store::ApprovalRequest> {
        match self.request(Request::GetApproval { approval_id })? {
            Response::Approval { approval } => Ok(approval),
            _ => Err(ClientError::UnexpectedResponse("Approval")),
        }
    }

    pub fn answer_approval(
        &mut self,
        approval_id: String,
        decision: String,
        operation_key: String,
        arguments: Vec<ApprovalArgument>,
    ) -> Result<store::ApprovalRequest> {
        match self.request(Request::AnswerApproval {
            approval_id,
            decision,
            operation_key,
            arguments,
        })? {
            Response::ApprovalResolved { approval } => Ok(approval),
            _ => Err(ClientError::UnexpectedResponse("ApprovalResolved")),
        }
    }

    pub fn interrupt_turn(&mut self, session_id: SessionId, turn_id: String) -> Result<()> {
        match self.request(Request::InterruptTurn {
            session_id,
            turn_id,
        })? {
            Response::TurnInterrupted { .. } => Ok(()),
            _ => Err(ClientError::UnexpectedResponse("TurnInterrupted")),
        }
    }

    pub fn stop_session(&mut self, session_id: SessionId) -> Result<bool> {
        match self.request(Request::StopSession { session_id })? {
            Response::Stopped { .. } => Ok(true),
            _ => Err(ClientError::UnexpectedResponse("Stopped")),
        }
    }

    pub fn list_controls(&mut self, session_id: SessionId) -> Result<Vec<ControlState>> {
        match self.request(Request::ListControls { session_id })? {
            Response::Controls { controls } => Ok(controls),
            _ => Err(ClientError::UnexpectedResponse("Controls")),
        }
    }

    pub fn set_control(
        &mut self,
        session_id: SessionId,
        name: String,
        value: String,
        scope: String,
    ) -> Result<ControlState> {
        match self.request(Request::SetControl {
            session_id,
            name,
            value,
            scope,
        })? {
            Response::ControlChanged { control } => Ok(control),
            _ => Err(ClientError::UnexpectedResponse("ControlChanged")),
        }
    }
}

pub struct TimelineSubscription {
    host: Host,
    session_id: SessionId,
    receiver: mpsc::Receiver<Result<Event>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cursor: Arc<std::sync::atomic::AtomicU64>,
}

impl TimelineSubscription {
    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            host: self.host.clone(),
            session_id: self.session_id.clone(),
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    pub fn recv(&self) -> Result<Event> {
        let event = self
            .receiver
            .recv()
            .map_err(|_| ClientError::SubscriptionClosed)??;
        persist_saved_cursor(&self.host, &self.session_id, event.seq)?;
        self.cursor.store(event.seq, Ordering::SeqCst);
        Ok(event)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event> {
        let event = self
            .receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    ClientError::Transport("timeline receive timed out".into())
                }
                mpsc::RecvTimeoutError::Disconnected => ClientError::SubscriptionClosed,
            })??;
        persist_saved_cursor(&self.host, &self.session_id, event.seq)?;
        self.cursor.store(event.seq, Ordering::SeqCst);
        Ok(event)
    }

    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for TimelineSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A timeline gap or permanent protocol error requires intervention, not a
/// retry loop using the same invalid cursor.
fn recoverable_subscription_error(error: &ClientError) -> bool {
    match error {
        ClientError::Transport(_) => true,
        ClientError::Api { code, .. } => matches!(code.as_str(), "Backpressure" | "Timeout"),
        _ => false,
    }
}

/// Deliver errors even when the bounded channel is full. Blocking is
/// cancelable so dropping an idle GUI subscription never deadlocks.
fn notify_subscription_error(
    sender: &mpsc::SyncSender<Result<Event>>,
    cancelled: &AtomicBool,
    error: ClientError,
) -> bool {
    let mut pending = Err(error);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return false;
        }
        match sender.try_send(pending) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(value)) => {
                pending = value;
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Apply bounded backpressure without dropping an accepted sequence.
/// The worker cursor advances only once the event is enqueued; the public
/// acknowledged cursor advances only after recv() persists delivery.
fn push_subscription_event(
    sender: &mpsc::SyncSender<Result<Event>>,
    cancelled: &AtomicBool,
    event: Event,
) -> Result<bool> {
    let start = std::time::Instant::now();
    let mut pending = Ok(event);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(false);
        }
        match sender.try_send(pending) {
            Ok(()) => return Ok(true),
            Err(mpsc::TrySendError::Disconnected(_)) => return Ok(false),
            Err(mpsc::TrySendError::Full(value)) => {
                if start.elapsed() >= Duration::from_secs(5) {
                    return Err(ClientError::Transport(
                        "timeline consumer backpressure; reconnect from last enqueued cursor".into(),
                    ));
                }
                pending = value;
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn subscription_worker(
    host: Host,
    session_id: SessionId,
    mut cursor: u64,
    sender: mpsc::SyncSender<Result<Event>>,
    cancelled: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_millis(100);
    while !cancelled.load(Ordering::SeqCst) {
        let result = subscription_connection(
            &host,
            &session_id,
            &mut cursor,
            &sender,
            &cancelled,
        );
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        if let Err(error) = result {
            let retry = recoverable_subscription_error(&error);
            if !notify_subscription_error(&sender, &cancelled, error) || !retry {
                break;
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        let until = std::time::Instant::now() + backoff;
        while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            thread::sleep(Duration::from_millis(25));
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn subscription_connection(
    host: &Host,
    session_id: &SessionId,
    cursor: &mut u64,
    sender: &mpsc::SyncSender<Result<Event>>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let mut client = FleetClient::connect(host.clone())?;
    client.next_request_id += 1;
    let id = format!("sdk-{}", client.next_request_id);
    write_frame(
        &mut client.writer,
        &Frame::Request {
            request_id: id.clone(),
            request: Request::SubscribeTimeline {
                session_id: session_id.clone(),
                since_seq: *cursor,
            },
        },
    )
    .map_err(|error| ClientError::Transport(error.to_string()))?;
    let mut reader = client.reader.take().expect("client reader present");
    let (frame_sender, frame_receiver) = mpsc::sync_channel(1);
    let reader_thread = thread::Builder::new()
        .name("fleet-timeline-reader".into())
        .spawn(move || loop {
            let frame = match read_frame(&mut reader) {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) => Err(ClientError::Transport(
                    "subscription connection closed".into(),
                )),
                Err(error) => Err(ClientError::Transport(error.to_string())),
            };
            let done = frame.is_err();
            if frame_sender.send(frame).is_err() || done {
                break;
            }
        })
        .map_err(|error| ClientError::Transport(error.to_string()))?;
    let mut expected = *cursor + 1;
    let result = loop {
        if cancelled.load(Ordering::SeqCst) {
            break Ok(());
        }
        let frame = match frame_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(frame)) => frame,
            Ok(Err(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(ClientError::Transport("timeline reader stopped".into()))
            }
        };
        match frame {
            Frame::Event {
                session_id: event_session,
                seq,
                event,
            } if event_session == *session_id => {
                if seq <= *cursor {
                    continue;
                }
                if seq != expected {
                    break Err(ClientError::RetentionGap {
                        expected,
                        received: seq,
                    });
                }
                match push_subscription_event(sender, cancelled, event) {
                    Ok(true) => {
                        *cursor = seq;
                        expected = seq + 1;
                    }
                    Ok(false) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
            Frame::Response { request_id, result } if request_id == id => {
                match result.map_err(|error| ClientError::Api {
                    code: error.code,
                    message: error.message,
                })? {
                    Response::Subscribed { .. } => {}
                    _ => break Err(ClientError::UnexpectedResponse("Subscribed")),
                }
            }
            Frame::Notice { code, message, .. } => {
                break Err(ClientError::Api { code, message });
            }
            Frame::Response { .. } | Frame::Event { .. } => continue,
            Frame::Request { .. } => {
                break Err(ClientError::Protocol(
                    "unexpected request from Fleet server".into(),
                ))
            }
        }
    };
    client.disconnect();
    drop(frame_receiver);
    let _ = reader_thread.join();
    result
}

fn operation_journal_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FLEET_OPERATION_JOURNAL") {
        return Ok(PathBuf::from(path));
    }
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| ClientError::Transport("HOME or XDG_STATE_HOME required".into()))?;
    Ok(root.join("gentle-fleet/operations.jsonl"))
}

fn cursor_journal_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FLEET_CURSOR_JOURNAL") {
        return Ok(PathBuf::from(path));
    }
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| ClientError::State("HOME or XDG_STATE_HOME required".into()))?;
    Ok(root.join("gentle-fleet/cursors.jsonl"))
}

fn host_cursor_key(host: &Host) -> String {
    match host {
        Host::Local => "local".into(),
        Host::Ssh(alias) => format!("ssh:{alias}"),
    }
}

fn load_saved_cursor(host: &Host, session_id: &str) -> Result<u64> {
    let path = cursor_journal_path()?;
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(ClientError::State(format!("cursor journal: {error}"))),
    };
    let host_key = host_cursor_key(host);
    let mut cursor = 0;
    for line in content.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).map_err(|error| ClientError::Protocol(error.to_string()))?;
        if record["host"] == host_key && record["session_id"] == session_id {
            cursor = record["seq"].as_u64().unwrap_or(cursor).max(cursor);
        }
    }
    Ok(cursor)
}

fn persist_saved_cursor(host: &Host, session_id: &str, seq: u64) -> Result<()> {
    let path = cursor_journal_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::State("cursor journal needs a parent directory".into()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| ClientError::State(format!("cursor journal: {error}")))?;
    #[cfg(unix)]
    if std::env::var_os("FLEET_CURSOR_JOURNAL").is_none() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| ClientError::State(format!("cursor journal: {error}")))?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ClientError::State(
                "cursor journal is not a regular file".into(),
            ));
        }
    }
    let mut line = serde_json::to_vec(&serde_json::json!({
        "host": host_cursor_key(host),
        "session_id": session_id,
        "seq": seq,
    }))
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    line.push(b'\n');
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| ClientError::State(format!("cursor journal: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file
            .metadata()
            .map_err(|error| ClientError::State(error.to_string()))?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(ClientError::State(
                "cursor journal permissions are too permissive".into(),
            ));
        }
    }
    file.write_all(&line)
        .and_then(|_| file.sync_data())
        .map_err(|error| ClientError::State(format!("cursor journal: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(seq: u64) -> Event {
        Event {
            event_id: format!("evt_{seq}"),
            session_id: "ses_test".into(),
            seq,
            created_at_ms: 0,
            kind: fleet_domain::EventKind::TurnStarted { turn_id: "turn_test".into() },
        }
    }

    #[test]
    fn queued_events_do_not_advance_consumed_cursor() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cursor = Arc::new(std::sync::atomic::AtomicU64::new(5));
        let subscription = TimelineSubscription {
            host: Host::Local,
            session_id: "ses_test".into(),
            receiver,
            cancelled: Arc::clone(&cancelled),
            worker: None,
            cursor,
        };
        assert!(push_subscription_event(&sender, &cancelled, sample_event(6)).unwrap());
        assert!(push_subscription_event(&sender, &cancelled, sample_event(7)).unwrap());
        assert_eq!(subscription.last_sequence(), 5, "queued != consumed");
        drop(subscription);
    }

    #[test]
    fn bounded_queue_waits_for_slow_consumer_without_losing_events() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(push_subscription_event(&sender, &cancelled, sample_event(1)).unwrap());
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            push_subscription_event(&sender, &worker_cancelled, sample_event(2))
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(receiver.recv().unwrap().unwrap().seq, 1);
        assert!(worker.join().unwrap().unwrap());
        assert_eq!(receiver.recv().unwrap().unwrap().seq, 2);
    }

    #[test]
    fn full_queue_does_not_discard_terminal_gap_error() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        sender.try_send(Ok(sample_event(1))).unwrap();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            notify_subscription_error(
                &sender,
                &worker_cancelled,
                ClientError::RetentionGap { expected: 2, received: 4 },
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(receiver.recv().unwrap().unwrap().seq, 1);
        assert!(worker.join().unwrap());
        assert!(matches!(
            receiver.recv().unwrap(),
            Err(ClientError::RetentionGap { expected: 2, received: 4 })
        ));
        assert!(!recoverable_subscription_error(&ClientError::RetentionGap {
            expected: 2,
            received: 4,
        }));
    }

    #[test]
    fn same_session_id_is_scoped_by_host() {
        let a = SessionIdentity {
            host: Host::Ssh("alpha".into()),
            session_id: "ses_1".into(),
        };
        let b = SessionIdentity {
            host: Host::Ssh("beta".into()),
            session_id: "ses_1".into(),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn cursor_namespaces_are_host_specific() {
        assert_ne!(
            host_cursor_key(&Host::Ssh("alpha".into())),
            host_cursor_key(&Host::Ssh("beta".into()))
        );
        assert_ne!(
            host_cursor_key(&Host::Local),
            host_cursor_key(&Host::Ssh("local".into()))
        );
    }
}
