use std::time::{Duration, Instant};

use acadctl_rpc::{DrawingOutcome as RpcDrawingOutcome, ExecMode as RpcExecMode, *};
use bytes::Bytes;
use futures_util::stream;
use tokio::sync::mpsc;

use super::{start, stop};
use crate::scheduler::CancelResult;

#[test]
fn reports_documents_and_stops_promptly() {
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    let house = test_drawing_path("house");
    let site = test_drawing_path("site");
    crate::scheduler::replace_document_snapshot(vec![
        crate::ffi::NativeDocSnapshot {
            document_token: 1,
            database_token: 101,
            name: house.as_str().into(),
            named: true,
            modified: false,
            read_only: false,
        },
        crate::ffi::NativeDocSnapshot {
            document_token: 2,
            database_token: 102,
            name: site.as_str().into(),
            named: true,
            modified: true,
            read_only: true,
        },
    ]);
    start().unwrap();
    assert!(
        acadctl_rpc::discover()
            .unwrap()
            .contains(&acadctl_rpc::ProcessId::new(std::process::id()).unwrap())
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let client = runtime.block_on(async {
        let mut client = acadctl_rpc::connect_documents(
            acadctl_rpc::ProcessId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        let listed = client.list(ListRequest {}).await.unwrap().into_inner();
        assert_eq!(listed.documents.len(), 2);
        assert_eq!(listed.documents[0].id.len(), 4);
        assert_eq!(
            listed.documents[0].display_name,
            house.as_path().file_name().unwrap()
        );
        assert_eq!(
            listed.documents[0].file_path.as_deref(),
            Some(house.as_str())
        );
        assert!(!listed.documents[0].modified);
        assert!(!listed.documents[0].read_only);
        assert_eq!(listed.documents[1].id.len(), 4);
        assert_ne!(listed.documents[0].id, listed.documents[1].id);
        assert_eq!(
            listed.documents[1].display_name,
            site.as_path().file_name().unwrap()
        );
        assert_eq!(
            listed.documents[1].file_path.as_deref(),
            Some(site.as_str())
        );
        assert!(listed.documents[1].modified);
        assert!(listed.documents[1].read_only);

        let opened = client
            .open(OpenRequest::from(house.clone()))
            .await
            .unwrap()
            .into_inner()
            .document
            .unwrap();
        assert_eq!(opened.id, listed.documents[0].id);

        let saved = client
            .save(SaveRequest {
                id: opened.id.clone(),
            })
            .await
            .unwrap()
            .into_inner()
            .document
            .unwrap();
        assert_eq!(saved.id, opened.id);
        assert!(!saved.modified);

        let mut undo_client = client.clone();
        let undo_id = opened.id.clone();
        let undo_response =
            tokio::spawn(async move { undo_client.undo(HistoryRequest { id: undo_id }).await });
        let undo_action = next_native_action().await;
        assert_eq!(undo_action.kind, crate::ffi::NativeActionKind::Undo);
        crate::scheduler::replace_document_snapshot(vec![
            crate::ffi::NativeDocSnapshot {
                document_token: 1,
                database_token: 101,
                name: house.as_str().into(),
                named: true,
                modified: true,
                read_only: false,
            },
            crate::ffi::NativeDocSnapshot {
                document_token: 2,
                database_token: 102,
                name: site.as_str().into(),
                named: true,
                modified: true,
                read_only: true,
            },
        ]);
        crate::scheduler::complete_native_action(
            undo_action.job_id,
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
            .document
            .unwrap();
        assert_eq!(undone.id, opened.id);
        assert!(undone.modified);

        let mut redo_client = client.clone();
        let redo_id = opened.id.clone();
        let redo_response =
            tokio::spawn(async move { redo_client.redo(HistoryRequest { id: redo_id }).await });
        let redo_action = next_native_action().await;
        assert_eq!(redo_action.kind, crate::ffi::NativeActionKind::Redo);
        crate::scheduler::replace_document_snapshot(vec![
            crate::ffi::NativeDocSnapshot {
                document_token: 1,
                database_token: 101,
                name: house.as_str().into(),
                named: true,
                modified: false,
                read_only: false,
            },
            crate::ffi::NativeDocSnapshot {
                document_token: 2,
                database_token: 102,
                name: site.as_str().into(),
                named: true,
                modified: true,
                read_only: true,
            },
        ]);
        crate::scheduler::complete_native_action(
            redo_action.job_id,
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
            .document
            .unwrap();
        assert_eq!(redone.id, opened.id);
        assert!(!redone.modified);

        let close_error = client
            .close(CloseRequest {
                id: listed.documents[1].id.clone(),
                discard: false,
            })
            .await
            .unwrap_err();
        assert_eq!(close_error.code(), tonic::Code::FailedPrecondition);
        assert!(close_error.message().contains("has unsaved changes"));
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

async fn next_native_action() -> crate::ffi::NativeAction {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);

    loop {
        let action = crate::scheduler::take_native_action();

        if action.kind != crate::ffi::NativeActionKind::None {
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
    crate::scheduler::replace_document_snapshot(vec![crate::ffi::NativeDocSnapshot {
        document_token: 1,
        database_token: 101,
        name: "/tmp/house.dwg".into(),
        named: true,
        modified: false,
        read_only: false,
    }]);
    let document_id = crate::scheduler::list().unwrap()[0].id;
    start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut client = acadctl_rpc::connect_execution(
            acadctl_rpc::ProcessId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        execute_and_cancel(
            &mut client,
            document_id,
            Bytes::from(vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES]),
        )
        .await;

        let mut with_bom = Vec::with_capacity(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3);
        with_bom.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        with_bom.resize(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3, b'x');
        execute_and_cancel(&mut client, document_id, Bytes::from(with_bom)).await;

        let request = execution_request(
            document_id,
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
fn dropping_the_rpc_stream_detaches_without_cancelling_the_job() {
    let _test = crate::scheduler::TEST_LOCK.blocking_lock();
    crate::scheduler::replace_document_snapshot(vec![crate::ffi::NativeDocSnapshot {
        document_token: 1,
        database_token: 101,
        name: "/tmp/house.dwg".into(),
        named: true,
        modified: false,
        read_only: false,
    }]);
    let document_id = crate::scheduler::list().unwrap()[0].id;
    start().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut client = acadctl_rpc::connect_execution(
            acadctl_rpc::ProcessId::new(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(execution_request(document_id, Bytes::from_static(b"form")))
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
        assert_eq!(action.kind, crate::ffi::NativeActionKind::QueueExecDriver);
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id).kind(),
            crate::exec::ExecStepKind::BeginUndoGroup
        );
        assert!(crate::scheduler::complete_execution_step(
            action.job_id,
            successful_step()
        ));
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id).kind(),
            crate::exec::ExecStepKind::EvaluateForm
        );
        assert_eq!(
            crate::scheduler::cancel_execution(action.job_id),
            CancelResult::Accepted
        );
        assert!(crate::scheduler::complete_execution_step(
            action.job_id,
            successful_step()
        ));
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id).kind(),
            crate::exec::ExecStepKind::RollbackUndoGroup
        );
        assert!(crate::scheduler::complete_execution_step(
            action.job_id,
            successful_step()
        ));
        assert_eq!(
            crate::scheduler::take_execution_step(action.job_id).kind(),
            crate::exec::ExecStepKind::Done
        );
        crate::scheduler::complete_native_action(
            action.job_id,
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
    document_id: DocId,
    source: Bytes,
) {
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(execution_request(document_id, source))
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

fn execution_request(document_id: DocId, source: Bytes) -> ExecClientMessage {
    ExecClientMessage {
        message: Some(exec_client_message::Message::Request(ExecRequest::new(
            document_id,
            RpcExecMode::Exec,
            "<stdin>".into(),
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
