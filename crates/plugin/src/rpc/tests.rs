use std::future::Future;
use std::time::{Duration, Instant};

use acadctl_rpc::{DrawingOutcome as RpcDrawingOutcome, ExecMode as RpcExecMode, *};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use tokio::sync::mpsc;

use super::{start, stop};
use crate::scheduler::CancelResult;

struct RpcTest {
    runtime: tokio::runtime::Runtime,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl RpcTest {
    fn start(drawings: Vec<crate::ffi::NativeDocumentSnapshot>) -> Self {
        let lock = crate::scheduler::TEST_LOCK.blocking_lock();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        crate::scheduler::replace_drawing_snapshot(drawings);
        start().unwrap();

        Self {
            runtime,
            _lock: lock,
        }
    }

    fn with_drawing(drawing: crate::ffi::NativeDocumentSnapshot) -> (Self, DrawingId) {
        let test = Self::start(vec![drawing]);
        let drawing_id = crate::scheduler::list().unwrap()[0].id;
        (test, drawing_id)
    }

    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

impl Drop for RpcTest {
    fn drop(&mut self) {
        stop();
    }
}

struct TestDrawingPath(DrawingPath);

impl TestDrawingPath {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "acadctl-rpc-server-{}-{name}.dwg",
            std::process::id()
        ));
        std::fs::write(&path, []).unwrap();
        Self(DrawingPath::canonicalize(path).unwrap())
    }

    fn path(&self) -> &DrawingPath {
        &self.0
    }
}

impl Drop for TestDrawingPath {
    fn drop(&mut self) {
        std::fs::remove_file(self.0.as_path()).unwrap();
    }
}

fn drawing_snapshot(
    document_token: usize,
    database_token: usize,
    name: impl Into<String>,
    modified: bool,
    read_only: bool,
) -> crate::ffi::NativeDocumentSnapshot {
    crate::ffi::NativeDocumentSnapshot {
        document_token,
        database_token,
        name: name.into(),
        named: true,
        modified,
        read_only,
    }
}

fn source_name(value: &str) -> SourceName {
    SourceName::new(value).unwrap()
}

async fn drawing_client() -> acadctl_rpc::DrawingServiceClient<tonic::transport::Channel> {
    acadctl_rpc::connect_drawings(acadctl_rpc::InstanceId::new(std::process::id()).unwrap())
        .await
        .unwrap()
}

async fn execution_client() -> acadctl_rpc::ExecServiceClient<tonic::transport::Channel> {
    acadctl_rpc::connect_execution(acadctl_rpc::InstanceId::new(std::process::id()).unwrap())
        .await
        .unwrap()
}

#[test]
fn list_serializes_a_drawing_snapshot() {
    let site = TestDrawingPath::new("list-site");
    let test = RpcTest::start(vec![drawing_snapshot(
        2,
        102,
        site.path().as_str(),
        true,
        true,
    )]);

    test.block_on(async {
        let mut client = drawing_client().await;
        let listed = client.list(ListRequest {}).await.unwrap().into_inner();

        assert_eq!(listed.drawings.len(), 1);
        assert!(DrawingId::try_from(listed.drawings[0].id).is_ok());
        assert_eq!(
            listed.drawings[0].display_name,
            site.path().as_path().file_name().unwrap()
        );
        assert_eq!(
            listed.drawings[0].file_path.as_deref(),
            Some(site.path().as_str())
        );
        assert!(listed.drawings[0].modified);
        assert!(listed.drawings[0].read_only);
    });
}

#[test]
fn open_returns_an_already_open_drawing() {
    let house = TestDrawingPath::new("open-house");
    let (test, drawing_id) = RpcTest::with_drawing(drawing_snapshot(
        1,
        101,
        house.path().as_str(),
        false,
        false,
    ));

    test.block_on(async {
        let mut client = drawing_client().await;
        let opened = client
            .open(OpenRequest::from(house.path().clone()))
            .await
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();

        assert_eq!(opened.id, drawing_id.into());
    });
}

#[test]
fn save_returns_the_published_drawing() {
    let house = TestDrawingPath::new("save-house");
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, house.path().as_str(), true, false));

    test.block_on(async {
        let mut client = drawing_client().await;
        let response = tokio::spawn(async move {
            client
                .save(SaveRequest {
                    drawing_id: drawing_id.into(),
                    path: None,
                })
                .await
        });
        let action = next_native_action().await;
        assert_eq!(action.kind(), crate::ffi::NativeActionKind::Save);
        crate::scheduler::replace_drawing_snapshot(vec![drawing_snapshot(
            1,
            101,
            house.path().as_str(),
            false,
            false,
        )]);
        complete_native_action_success(&action);

        let saved = response
            .await
            .unwrap()
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(saved.id, drawing_id.into());
        assert!(!saved.modified);
    });
}

#[test]
fn undo_routes_to_undo_and_returns_the_published_drawing() {
    let house = TestDrawingPath::new("undo-house");
    let (test, drawing_id) = RpcTest::with_drawing(drawing_snapshot(
        1,
        101,
        house.path().as_str(),
        false,
        false,
    ));

    test.block_on(async {
        let mut client = drawing_client().await;
        let response = tokio::spawn(async move {
            client
                .undo(HistoryRequest {
                    drawing_id: drawing_id.into(),
                })
                .await
        });
        let action = next_native_action().await;
        assert_eq!(action.kind(), crate::ffi::NativeActionKind::Undo);
        crate::scheduler::replace_drawing_snapshot(vec![drawing_snapshot(
            1,
            101,
            house.path().as_str(),
            true,
            false,
        )]);
        complete_native_action_success(&action);

        let undone = response
            .await
            .unwrap()
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(undone.id, drawing_id.into());
        assert!(undone.modified);
    });
}

#[test]
fn redo_routes_to_redo_and_returns_the_published_drawing() {
    let house = TestDrawingPath::new("redo-house");
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, house.path().as_str(), true, false));

    test.block_on(async {
        let mut client = drawing_client().await;
        let response = tokio::spawn(async move {
            client
                .redo(HistoryRequest {
                    drawing_id: drawing_id.into(),
                })
                .await
        });
        let action = next_native_action().await;
        assert_eq!(action.kind(), crate::ffi::NativeActionKind::Redo);
        crate::scheduler::replace_drawing_snapshot(vec![drawing_snapshot(
            1,
            101,
            house.path().as_str(),
            false,
            false,
        )]);
        complete_native_action_success(&action);

        let redone = response
            .await
            .unwrap()
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(redone.id, drawing_id.into());
        assert!(!redone.modified);
    });
}

#[test]
fn close_reports_typed_unsaved_changes() {
    let house = TestDrawingPath::new("dirty-house");
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, house.path().as_str(), true, false));

    test.block_on(async {
        let mut client = drawing_client().await;
        let error = client
            .close(CloseRequest {
                drawing_id: drawing_id.into(),
                discard: false,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(
            DrawingError::from_status(&error),
            Some(DrawingError {
                kind: DrawingErrorKind::UnsavedChanges as i32,
                drawing_id: Some(drawing_id.into()),
            })
        );
    });
}

#[test]
fn stop_returns_promptly_with_a_connected_client() {
    let test = RpcTest::start(Vec::new());
    let client = test.block_on(drawing_client());

    let started = Instant::now();
    stop();

    assert!(started.elapsed() < Duration::from_secs(1));
    drop(client);
}

async fn next_native_action() -> crate::scheduler::NativeAction {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);

    loop {
        let action = crate::scheduler::take_native_action();

        if action.kind() != crate::ffi::NativeActionKind::None {
            return action;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "RPC did not enqueue a native action"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn complete_native_action_success(action: &crate::scheduler::NativeAction) {
    crate::scheduler::complete_native_action(
        action.job_id(),
        crate::ffi::NativeActionResult {
            kind: crate::ffi::NativeActionResultKind::Success,
            native_status: 0,
            native_detail: String::new(),
        },
    );
}

#[test]
fn accepts_source_at_the_size_limit() {
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, "/tmp/house.dwg", false, false));

    test.block_on(async {
        let mut client = execution_client().await;
        let acknowledgements = accept_and_cancel(
            &mut client,
            drawing_id,
            Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES]),
            1,
        )
        .await;
        assert_eq!(acknowledgements, 1);
    });
}

#[test]
fn accepts_the_size_limit_after_a_utf8_bom() {
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, "/tmp/house.dwg", false, false));
    let mut source = Vec::with_capacity(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3);
    source.extend_from_slice(&[0xef, 0xbb, 0xbf]);
    source.resize(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3, b'x');

    test.block_on(async {
        let mut client = execution_client().await;
        let acknowledgements =
            accept_and_cancel(&mut client, drawing_id, Bytes::from(source), 1).await;
        assert_eq!(acknowledgements, 1);
    });
}

#[test]
fn rejects_an_oversized_source_before_acceptance() {
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, "/tmp/house.dwg", false, false));

    test.block_on(async {
        let mut client = execution_client().await;
        let request = execution_request(
            drawing_id,
            Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 1]),
        );
        let mut response = client
            .execute(stream::iter([request]))
            .await
            .unwrap()
            .into_inner();
        let event = response.message().await.unwrap().unwrap();
        let Some(exec_server_event::Event::Finished(finished)) = event.event else {
            panic!("oversized source must fail before acceptance");
        };
        let Some(exec_outcome::Outcome::Failure(failure)) = finished.outcome.unwrap().outcome
        else {
            panic!("oversized source must produce a structured failure");
        };

        assert!(failure.message.contains("4 MiB"));
        assert_eq!(
            failure.drawing_outcome,
            RpcDrawingOutcome::NotStarted as i32
        );
        assert!(response.message().await.unwrap().is_none());
    });
}

#[test]
fn duplicate_cancellation_produces_one_acknowledgement() {
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, "/tmp/house.dwg", false, false));

    test.block_on(async {
        let mut client = execution_client().await;
        let acknowledgements =
            accept_and_cancel(&mut client, drawing_id, Bytes::from_static(b"form"), 2).await;
        assert_eq!(acknowledgements, 1);
    });
}

#[test]
fn terminal_execution_response_holds_capacity_until_observed() {
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    let mut reservations =
        std::iter::from_fn(crate::scheduler::try_reserve_execution).collect::<Vec<_>>();
    assert!(!reservations.is_empty());
    let reservation = reservations.pop().unwrap();

    let mut response = super::exec::terminal_response(
        reservation,
        crate::exec::ExecFailure::not_started("invalid request".to_owned()),
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        assert!(crate::scheduler::try_reserve_execution().is_none());
        assert!(response.next().await.unwrap().is_ok());
        assert!(crate::scheduler::try_reserve_execution().is_some());
        assert!(response.next().await.is_none());
    });
}

#[test]
fn dropping_the_rpc_stream_detaches_without_cancelling_the_job() {
    let (test, drawing_id) =
        RpcTest::with_drawing(drawing_snapshot(1, 101, "/tmp/house.dwg", false, false));

    test.block_on(async {
        let mut client = execution_client().await;
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(execution_request(drawing_id, Bytes::from_static(b"form")))
            .await
            .unwrap();
        let outbound = stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|message| (message, receiver))
        });
        let mut response = client.execute(outbound).await.unwrap().into_inner();
        assert!(matches!(
            response.message().await.unwrap().unwrap().event,
            Some(exec_server_event::Event::Accepted(_))
        ));
        drop(response);
        drop(sender);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let action = crate::scheduler::take_native_action();
        assert_eq!(action.kind(), crate::ffi::NativeActionKind::QueueExecDriver);
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id()).kind(),
            crate::exec::ExecStepKind::BeginUndoGroup
        );
        assert!(crate::scheduler::complete_execution_step(
            action.job_id(),
            successful_step()
        ));
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id()).kind(),
            crate::exec::ExecStepKind::EvaluateForm
        );
        assert_eq!(
            crate::scheduler::cancel_execution(action.job_id()),
            CancelResult::Accepted
        );
        finish_cancelled_execution(&action);
    });
}

async fn accept_and_cancel(
    client: &mut acadctl_rpc::ExecServiceClient<tonic::transport::Channel>,
    drawing_id: DrawingId,
    source: Bytes,
    cancellation_requests: usize,
) -> usize {
    let (sender, receiver) = mpsc::channel(cancellation_requests.max(1));
    sender
        .send(execution_request(drawing_id, source))
        .await
        .unwrap();
    let outbound = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|message| (message, receiver))
    });
    let mut response = client.execute(outbound).await.unwrap().into_inner();
    let accepted = response.message().await.unwrap().unwrap();
    assert!(matches!(
        accepted.event,
        Some(exec_server_event::Event::Accepted(_))
    ));

    for _ in 0..cancellation_requests {
        sender
            .send(ExecClientMessage {
                message: Some(exec_client_message::Message::Cancel(
                    acadctl_rpc::ExecCancelRequest {},
                )),
            })
            .await
            .unwrap();
    }

    let mut acknowledgements = 0;
    loop {
        let event = response
            .message()
            .await
            .expect("execution stream read failed")
            .expect("execution stream ended before its terminal event");

        match event.event {
            Some(exec_server_event::Event::CancelAcknowledgement(acknowledgement)) => {
                assert_eq!(
                    acknowledgement.disposition,
                    ExecCancelDisposition::Accepted as i32
                );
                acknowledgements += 1;
            }
            Some(exec_server_event::Event::Finished(finished)) => {
                assert!(matches!(
                    finished.outcome.unwrap().outcome,
                    Some(exec_outcome::Outcome::Cancelled(_))
                ));
                break;
            }
            Some(exec_server_event::Event::Accepted(_))
            | Some(exec_server_event::Event::Output(_))
            | None => panic!("unexpected execution event"),
        }
    }

    assert!(
        response
            .message()
            .await
            .expect("execution stream read failed after its terminal event")
            .is_none()
    );
    acknowledgements
}

fn finish_cancelled_execution(action: &crate::scheduler::NativeAction) {
    assert!(crate::scheduler::complete_execution_step(
        action.job_id(),
        successful_step()
    ));
    assert_eq!(
        crate::scheduler::take_execution_step(action.job_id()).kind(),
        crate::exec::ExecStepKind::RollbackUndoGroup
    );
    assert!(crate::scheduler::complete_execution_step(
        action.job_id(),
        successful_step()
    ));
    assert_eq!(
        crate::scheduler::take_execution_step(action.job_id()).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action_success(action);
}

fn execution_request(drawing_id: DrawingId, source: Bytes) -> ExecClientMessage {
    ExecClientMessage {
        message: Some(exec_client_message::Message::Request(ExecRequest::new(
            drawing_id,
            RpcExecMode::Exec,
            source_name("<stdin>"),
            source,
        ))),
    }
}

fn successful_step() -> crate::exec::ExecStepResult {
    crate::exec::ExecStepResult {
        kind: crate::exec::ExecStepResultKind::Success,
        native_status: 0,
        lisp_errno: 0,
        detail: String::new(),
        bridge_symbols_clear_status: 0,
    }
}
