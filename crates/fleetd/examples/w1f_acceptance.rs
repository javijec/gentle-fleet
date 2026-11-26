//! Black-box W1-F fixture client. Only run against isolated FakeDriver sessions.
//! Uses the public wire, not daemon state, private tokens in logs or SQLite.
use std::io::{BufReader, BufWriter};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fleet_domain::store::{ApprovalRequest, ApprovalStatus, OperationStatus};
use fleet_domain::{Event, EventKind, TurnStatus};
use fleet_protocol::transport::{connect, expected_token, LocalStream};
use fleet_protocol::{read_frame, write_frame, ApiError, Frame, Request, Response, WIRE};

struct Client {
    reader: BufReader<LocalStream>,
    writer: BufWriter<LocalStream>,
}

impl Client {
    fn connect() -> Self {
        let stream = connect().expect("fixture daemon connection");
        let mut client = Self {
            reader: BufReader::new(stream.try_clone().expect("clone fixture stream")),
            writer: BufWriter::new(stream),
        };
        assert!(matches!(
            client.call(Request::Hello {
                client_id: "w1f-fixture".into(),
                token: expected_token().expect("fixture token"),
            }),
            Ok(Response::Hello { wire, .. }) if wire == WIRE
        ));
        client
    }

    fn send(&mut self, request: Request) {
        write_frame(
            &mut self.writer,
            &Frame::Request {
                request_id: "fixture-request".into(),
                request,
            },
        )
        .expect("send fixture request");
    }

    fn recv(&mut self) -> Frame {
        read_frame(&mut self.reader)
            .expect("read fixture frame")
            .expect("unexpected fixture EOF")
    }

    fn call(&mut self, request: Request) -> Result<Response, ApiError> {
        self.send(request);
        match self.recv() {
            Frame::Response { request_id, result } if request_id == "fixture-request" => result,
            other => panic!("unexpected fixture response: {other:?}"),
        }
    }

    fn timeline(&mut self, session: &str) -> Vec<Event> {
        match self
            .call(Request::ReadTimeline {
                session_id: session.into(),
                since_seq: 0,
                limit: 256,
            })
            .expect("read fixture timeline")
        {
            Response::Timeline { session_id, events } => {
                assert_eq!(session_id, session);
                for (index, event) in events.iter().enumerate() {
                    assert_eq!(event.session_id, session);
                    assert_eq!(event.seq, index as u64 + 1);
                    assert_eq!(event.event_id, format!("{session}-e{}", event.seq));
                }
                events
            }
            other => panic!("unexpected timeline: {other:?}"),
        }
    }

    fn subscribe(&mut self, session: &str, since: u64) -> (u64, Vec<Event>) {
        self.send(Request::SubscribeTimeline {
            session_id: session.into(),
            since_seq: since,
        });
        let mut events = Vec::new();
        loop {
            match self.recv() {
                Frame::Event { session_id, seq, event } => {
                    assert_eq!(session_id, session);
                    assert_eq!(seq, event.seq);
                    events.push(event);
                }
                Frame::Response { request_id, result: Ok(Response::Subscribed { session_id, latest_seq }) } => {
                    assert_eq!(request_id, "fixture-request");
                    assert_eq!(session_id, session);
                    return (latest_seq, events);
                }
                other => panic!("unexpected subscribe frame: {other:?}"),
            }
        }
    }
}

fn prompt(session: &str, text: &str, key: &str) -> Request {
    Request::SendPrompt {
        session_id: session.into(),
        text: text.into(),
        operation_key: key.into(),
    }
}

fn answer(approval: &ApprovalRequest, decision: &str, key: &str) -> Request {
    Request::AnswerApproval {
        approval_id: approval.approval_id.clone(),
        decision: decision.into(),
        operation_key: key.into(),
        arguments: vec![],
    }
}

fn race(requests: Vec<Request>) -> Vec<Result<Response, ApiError>> {
    // Connect before releasing the barrier, so the calls are genuinely
    // concurrent without sleeping or relying on connection startup order.
    let barrier = Arc::new(Barrier::new(requests.len() + 1));
    let workers: Vec<_> = requests
        .into_iter()
        .map(|request| {
            let mut client = Client::connect();
            let ready = Arc::clone(&barrier);
            std::thread::spawn(move || {
                ready.wait();
                client.call(request)
            })
        })
        .collect();
    barrier.wait();
    workers.into_iter().map(|worker| worker.join().unwrap()).collect()
}

fn expect_error(result: Result<Response, ApiError>, code: &str) {
    assert_eq!(result.expect_err("unexpected successful effect").code, code);
}

fn operation(client: &mut Client, key: &str, status: OperationStatus) {
    match client.call(Request::GetOperation { operation_key: key.into() }).unwrap() {
        Response::Operation { operation } => {
            assert_eq!(operation.operation_key, key);
            assert_eq!(operation.status, status);
        }
        other => panic!("unexpected operation response: {other:?}"),
    }
}

fn missing_operation(client: &mut Client, key: &str) {
    expect_error(client.call(Request::GetOperation { operation_key: key.into() }), "not_found");
}

fn pending(client: &mut Client, session: &str, turn: &str) -> ApprovalRequest {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match client.call(Request::ListApprovals { session_id: Some(session.into()) }).unwrap() {
            Response::Approvals { approvals } if !approvals.is_empty() => {
                assert_eq!(approvals.len(), 1);
                let approval = approvals.into_iter().next().unwrap();
                assert_eq!(approval.session_id, session);
                assert_eq!(approval.turn_id, turn);
                assert_eq!(approval.status, ApprovalStatus::Pending);
                assert!(approval.decision.is_none());
                assert!(client.timeline(session).iter().all(|event| !matches!(
                    event.kind,
                    EventKind::ToolStarted { .. } | EventKind::TurnCompleted { .. } | EventKind::ApprovalResolved { .. }
                )));
                return approval;
            }
            Response::Approvals { .. } => {}
            other => panic!("unexpected approvals response: {other:?}"),
        }
        assert!(Instant::now() < deadline, "approval was not observed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn receipt(response: &Response, session: &str) -> String {
    match response {
        Response::PromptAccepted { session_id, turn_id, .. } => {
            assert_eq!(session_id, session);
            turn_id.clone()
        }
        other => panic!("unexpected prompt receipt: {other:?}"),
    }
}

fn settled(
    client: &mut Client,
    approval: &ApprovalRequest,
    decision: &str,
    prompt_text: &str,
    prompt_key: &str,
    answer_key: &str,
) -> Vec<Event> {
    let allowed = decision == "once";
    let status = if allowed { TurnStatus::Completed } else { TurnStatus::Interrupted };
    let approval_status = if allowed { ApprovalStatus::Accepted } else { ApprovalStatus::Rejected };
    let deadline = Instant::now() + Duration::from_secs(15);
    let events = loop {
        let events = client.timeline(&approval.session_id);
        if events.iter().any(|e| matches!(e.kind, EventKind::TurnCompleted { .. })) {
            break events;
        }
        assert!(Instant::now() < deadline, "turn did not reach terminal");
        std::thread::sleep(Duration::from_millis(50));
    };
    let users: Vec<_> = events.iter().filter_map(|e| match &e.kind {
        EventKind::UserMessage { turn_id, text } => Some((turn_id, text)),
        _ => None,
    }).collect();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].0, &approval.turn_id);
    assert_eq!(users[0].1.as_str(), prompt_text);
    let terminals: Vec<_> = events.iter().filter_map(|e| match &e.kind {
        EventKind::TurnCompleted { turn_id, status } => Some((turn_id, status)),
        _ => None,
    }).collect();
    assert_eq!(terminals, vec![(&approval.turn_id, &status)]);
    let resolutions: Vec<_> = events.iter().filter_map(|e| match &e.kind {
        EventKind::ApprovalResolved { approval_id, turn_id, decision } => Some((approval_id, turn_id, decision.as_str())),
        _ => None,
    }).collect();
    assert_eq!(resolutions, vec![(&approval.approval_id, &approval.turn_id, decision)]);
    assert_eq!(events.iter().filter(|e| matches!(e.kind, EventKind::ToolStarted { .. })).count(), usize::from(allowed));
    match client.call(Request::GetApproval { approval_id: approval.approval_id.clone() }).unwrap() {
        Response::Approval { approval } => {
            assert_eq!(approval.status, approval_status);
            assert_eq!(approval.decision.as_deref(), Some(decision));
        }
        other => panic!("unexpected approval response: {other:?}"),
    }
    operation(client, prompt_key, if allowed { OperationStatus::Completed } else { OperationStatus::Failed });
    operation(client, answer_key, OperationStatus::Completed);
    events
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2, "expected two isolated fixture sessions");
    let (a, b) = (args[0].as_str(), args[1].as_str());
    assert_ne!(a, b);
    let mut client = Client::connect();

    // Confirm subscription through the server response before dispatch. The
    // existing debug-only forwarding delay holds this subscriber; the queue
    // limit of one forces an explicit overflow as the prompt emits events.
    let mut slow = Client::connect();
    let initial = client.timeline(a);
    let cutoff = initial.last().expect("session creation event").seq;
    assert_eq!(slow.subscribe(a, cutoff), (cutoff, vec![]));

    let candidates = [("alpha-fixture", "prompt-alpha"), ("beta-fixture", "prompt-beta")];
    let results = race(vec![
        prompt(a, candidates[0].0, candidates[0].1),
        prompt(a, candidates[1].0, candidates[1].1),
        prompt(b, "independent-fixture", "prompt-independent"),
    ]);
    let winner = match (&results[0], &results[1]) {
        (Ok(_), Err(error)) if error.code == "TurnAlreadyRunning" => 0,
        (Err(error), Ok(_)) if error.code == "TurnAlreadyRunning" => 1,
        other => panic!("expected exactly one turn winner: {other:?}"),
    };
    let won = results[winner].as_ref().unwrap();
    let turn_a = receipt(won, a);
    let turn_b = receipt(results[2].as_ref().unwrap(), b);
    assert_ne!(turn_a, turn_b);
    let (text, key) = candidates[winner];
    assert_eq!(client.call(prompt(a, text, key)).unwrap(), *won);
    expect_error(client.call(prompt(a, "changed-payload", key)), "Conflict");
    missing_operation(&mut client, candidates[1 - winner].1);
    println!("PASS: concurrent sessions, one active turn, idempotent receipt and conflict rejection");

    match slow.recv() {
        Frame::Notice { session_id: Some(session), code, .. } => {
            assert_eq!(session, a);
            assert_eq!(code, "Backpressure");
        }
        other => panic!("expected typed backpressure notice, got {other:?}"),
    }
    drop(slow);
    let approval_a = pending(&mut client, a, &turn_a);
    let approval_b = pending(&mut client, b, &turn_b);
    assert_ne!(approval_a.approval_id, approval_b.approval_id);

    let choices = [("once", "answer-allow"), ("reject", "answer-reject")];
    let answers = race(vec![
        answer(&approval_a, choices[0].0, choices[0].1),
        answer(&approval_a, choices[1].0, choices[1].1),
    ]);
    let winner = match (&answers[0], &answers[1]) {
        (Ok(Response::ApprovalResolved { .. }), Err(error)) if error.code == "Conflict" => 0,
        (Err(error), Ok(Response::ApprovalResolved { .. })) if error.code == "Conflict" => 1,
        other => panic!("expected exactly one approval winner: {other:?}"),
    };
    let (decision, answer_key) = choices[winner];
    missing_operation(&mut client, choices[1 - winner].1);
    // A's decision cannot resolve B. B is explicitly rejected.
    assert_eq!(pending(&mut client, b, &turn_b).approval_id, approval_b.approval_id);
    assert!(matches!(client.call(answer(&approval_b, "reject", "answer-independent")), Ok(Response::ApprovalResolved { .. })));
    let events_a = settled(&mut client, &approval_a, decision, text, key, answer_key);
    let events_b = settled(&mut client, &approval_b, "reject", "independent-fixture", "prompt-independent", "answer-independent");
    assert!(matches!(client.call(answer(&approval_a, decision, answer_key)), Ok(Response::ApprovalResolved { .. })));
    assert_eq!(client.timeline(a), events_a);
    println!("PASS: single approval winner, independent rejection and durable decisions without autoapproval");

    // Overflowed subscription must recover all events from its original
    // cursor. Compare typed events (including id, seq and payload), not text.
    let mut replay = Client::connect();
    let (latest, events) = replay.subscribe(a, cutoff);
    assert_eq!(latest, events_a.last().unwrap().seq);
    assert_eq!(events, events_a[initial.len()..]);
    assert_eq!(client.timeline(b), events_b);
    println!("PASS: explicit subscriber backpressure and exact durable replay; no cross-session events");
}
