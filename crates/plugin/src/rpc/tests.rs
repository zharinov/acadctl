use std::time::{Duration, Instant};

use acadctl_rpc::{DrawingOutcome as RpcDrawingOutcome, ExecMode as RpcExecMode, *};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use tokio::sync::mpsc;

use super::{start, stop};
use crate::scheduler::CancelResult;

fn source_name(value: &str) -> SourceName {
    SourceName::new(value).unwrap()
}

#[test]
fn reports_drawings_and_stops_promptly() {
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    let house = test_drawing_path("house");
    let site = test_drawing_path("site");
    crate::scheduler::replace_drawing_snapshot(vec![
        crate::ffi::NativeDocumentSnapshot {
            document_token: 1,
            database_token: 101,
            name: house.as_str().into(),
            named: true,
            modified: false,
            read_only: false,
        },
        crate::ffi::NativeDocumentSnapshot {
            document_token: 2,
            database_token: 102,
            name: site.as_str().into(),
            named: true,
            modified: true,
            read_only: true,
        },
    ]);
    start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let client = runtime.block_on(async {
        let mut client = acadctl_rpc::connect_drawings(
            acadctl_rpc::InstanceId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        let listed = client.list(ListRequest {}).await.unwrap().into_inner();
        assert_eq!(listed.drawings.len(), 2);
        assert!(DrawingId::try_from(listed.drawings[0].id).is_ok());
        assert_eq!(
            listed.drawings[0].display_name,
            house.as_path().file_name().unwrap()
        );
        assert_eq!(
            listed.drawings[0].file_path.as_deref(),
            Some(house.as_str())
        );
        assert!(!listed.drawings[0].modified);
        assert!(!listed.drawings[0].read_only);
        assert!(DrawingId::try_from(listed.drawings[1].id).is_ok());
        assert_ne!(listed.drawings[0].id, listed.drawings[1].id);
        assert_eq!(
            listed.drawings[1].display_name,
            site.as_path().file_name().unwrap()
        );
        assert_eq!(listed.drawings[1].file_path.as_deref(), Some(site.as_str()));
        assert!(listed.drawings[1].modified);
        assert!(listed.drawings[1].read_only);

        let opened = client
            .open(OpenRequest::from(house.clone()))
            .await
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(opened.id, listed.drawings[0].id);

        let saved = client
            .save(SaveRequest {
                drawing_id: opened.id,
                path: None,
            })
            .await
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(saved.id, opened.id);
        assert!(!saved.modified);

        let mut undo_client = client.clone();
        let undo_id = opened.id;
        let undo_response = tokio::spawn(async move {
            undo_client
                .undo(HistoryRequest {
                    drawing_id: undo_id,
                })
                .await
        });
        let undo_action = next_native_action().await;
        assert_eq!(undo_action.kind(), crate::ffi::NativeActionKind::Undo);
        crate::scheduler::replace_drawing_snapshot(vec![
            crate::ffi::NativeDocumentSnapshot {
                document_token: 1,
                database_token: 101,
                name: house.as_str().into(),
                named: true,
                modified: true,
                read_only: false,
            },
            crate::ffi::NativeDocumentSnapshot {
                document_token: 2,
                database_token: 102,
                name: site.as_str().into(),
                named: true,
                modified: true,
                read_only: true,
            },
        ]);
        crate::scheduler::complete_native_action(
            undo_action.job_id(),
            crate::ffi::NativeActionResult {
                kind: crate::ffi::NativeActionResultKind::Success,
                native_status: 0,
                native_detail: String::new(),
            },
        );
        let undone = undo_response
            .await
            .unwrap()
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(undone.id, opened.id);
        assert!(undone.modified);

        let mut redo_client = client.clone();
        let redo_id = opened.id;
        let redo_response = tokio::spawn(async move {
            redo_client
                .redo(HistoryRequest {
                    drawing_id: redo_id,
                })
                .await
        });
        let redo_action = next_native_action().await;
        assert_eq!(redo_action.kind(), crate::ffi::NativeActionKind::Redo);
        crate::scheduler::replace_drawing_snapshot(vec![
            crate::ffi::NativeDocumentSnapshot {
                document_token: 1,
                database_token: 101,
                name: house.as_str().into(),
                named: true,
                modified: false,
                read_only: false,
            },
            crate::ffi::NativeDocumentSnapshot {
                document_token: 2,
                database_token: 102,
                name: site.as_str().into(),
                named: true,
                modified: true,
                read_only: true,
            },
        ]);
        crate::scheduler::complete_native_action(
            redo_action.job_id(),
            crate::ffi::NativeActionResult {
                kind: crate::ffi::NativeActionResultKind::Success,
                native_status: 0,
                native_detail: String::new(),
            },
        );
        let redone = redo_response
            .await
            .unwrap()
            .unwrap()
            .into_inner()
            .drawing
            .unwrap();
        assert_eq!(redone.id, opened.id);
        assert!(!redone.modified);

        let close_error = client
            .close(CloseRequest {
                drawing_id: listed.drawings[1].id,
                discard: false,
            })
            .await
            .unwrap_err();
        assert_eq!(close_error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(
            DrawingError::from_status(&close_error),
            Some(DrawingError {
                kind: DrawingErrorKind::UnsavedChanges as i32,
                drawing_id: DrawingId::try_from(listed.drawings[1].id)
                    .ok()
                    .map(Into::into),
            })
        );
        client
    });

    let started = Instant::now();
    stop();
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(client);
    std::fs::remove_file(house.as_path()).unwrap();
    std::fs::remove_file(site.as_path()).unwrap();
}

fn test_drawing_path(name: &str) -> acadctl_rpc::DrawingPath {
    let path = std::env::temp_dir().join(format!(
        "acadctl-rpc-server-{}-{name}.dwg",
        std::process::id()
    ));
    std::fs::write(&path, []).unwrap();
    acadctl_rpc::DrawingPath::canonicalize(path).unwrap()
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

#[test]
fn execute_transport_preserves_the_four_mib_source_boundary() {
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    crate::scheduler::replace_drawing_snapshot(vec![crate::ffi::NativeDocumentSnapshot {
        document_token: 1,
        database_token: 101,
        name: "/tmp/house.dwg".into(),
        named: true,
        modified: false,
        read_only: false,
    }]);
    let drawing_id = crate::scheduler::list().unwrap()[0].id;
    start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut client = acadctl_rpc::connect_execution(
            acadctl_rpc::InstanceId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        execute_and_cancel(
            &mut client,
            drawing_id,
            Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES]),
        )
        .await;

        let mut with_bom = Vec::with_capacity(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3);
        with_bom.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        with_bom.resize(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3, b'x');
        execute_and_cancel(&mut client, drawing_id, Bytes::from(with_bom)).await;

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

    stop();
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
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    crate::scheduler::replace_drawing_snapshot(vec![crate::ffi::NativeDocumentSnapshot {
        document_token: 1,
        database_token: 101,
        name: "/tmp/house.dwg".into(),
        named: true,
        modified: false,
        read_only: false,
    }]);
    let drawing_id = crate::scheduler::list().unwrap()[0].id;
    start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut client = acadctl_rpc::connect_execution(
            acadctl_rpc::InstanceId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
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
        crate::scheduler::complete_native_action(
            action.job_id(),
            crate::ffi::NativeActionResult {
                kind: crate::ffi::NativeActionResultKind::Success,
                native_status: 0,
                native_detail: String::new(),
            },
        );
    });

    stop();
}

async fn execute_and_cancel(
    client: &mut acadctl_rpc::ExecServiceClient<tonic::transport::Channel>,
    drawing_id: DrawingId,
    source: Bytes,
) {
    let (sender, receiver) = mpsc::channel(2);
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

    for _ in 0..2 {
        sender
            .send(ExecClientMessage {
                message: Some(exec_client_message::Message::Cancel(
                    acadctl_rpc::ExecCancelRequest {},
                )),
            })
            .await
            .unwrap();
    }

    let mut cancel_acknowledgement_count = 0;
    let mut finished_seen = false;

    while let Some(event) = response.message().await.unwrap() {
        match event.event {
            Some(exec_server_event::Event::CancelAcknowledgement(acknowledgement)) => {
                assert_eq!(
                    acknowledgement.disposition,
                    ExecCancelDisposition::Accepted as i32
                );
                cancel_acknowledgement_count += 1;
            }
            Some(exec_server_event::Event::Finished(finished)) => {
                assert!(matches!(
                    finished.outcome.unwrap().outcome,
                    Some(exec_outcome::Outcome::Cancelled(_))
                ));
                finished_seen = true;
            }

            Some(exec_server_event::Event::Accepted(_))
            | Some(exec_server_event::Event::Output(_))
            | None => panic!("unexpected execution event"),
        }
    }

    assert_eq!(cancel_acknowledgement_count, 1);
    assert!(finished_seen);
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
