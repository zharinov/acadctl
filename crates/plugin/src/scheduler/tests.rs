use super::super::operation::interpret;
use super::*;
use crate::exec::ExecMode;
use crate::exec::value::writer::{ValueEvent, WriteResult};

#[test]
fn execution_finalization_classification_uses_native_facts_in_rust() {
    let clean = classify_execution_finalization(
        result(NativeActionResultKind::Success),
        ExecFinalizationObservation::default(),
    );
    assert_eq!(clean.result.kind, NativeActionResultKind::Success);
    assert!(!clean.quarantine);

    for observation in [
        ExecFinalizationObservation {
            undo_group_may_be_open: true,
            ..ExecFinalizationObservation::default()
        },
        ExecFinalizationObservation {
            bridge_symbols_may_be_retained: true,
            ..ExecFinalizationObservation::default()
        },
        ExecFinalizationObservation {
            staged_form_may_be_retained: true,
            ..ExecFinalizationObservation::default()
        },
        ExecFinalizationObservation {
            value_writer_active: true,
            ..ExecFinalizationObservation::default()
        },
        ExecFinalizationObservation {
            terminal_cleanup_failed: true,
            ..ExecFinalizationObservation::default()
        },
    ] {
        let retained = classify_execution_finalization(
            result(NativeActionResultKind::ExecBridgeFailed),
            observation,
        );
        assert_eq!(
            retained.result.kind,
            NativeActionResultKind::ExecBridgeFinalizationFailed
        );
        assert!(retained.quarantine);
    }

    let restore = classify_execution_finalization(
        result(NativeActionResultKind::DocContextRestoreFailed),
        ExecFinalizationObservation {
            undo_group_may_be_open: true,
            ..ExecFinalizationObservation::default()
        },
    );
    assert_eq!(
        restore.result.kind,
        NativeActionResultKind::DocContextRestoreFailed
    );
    assert!(restore.quarantine);

    let retained_symbols = classify_execution_finalization(
        result(NativeActionResultKind::ExecBridgeSymbolsClearFailed),
        ExecFinalizationObservation {
            bridge_symbols_may_be_retained: true,
            ..ExecFinalizationObservation::default()
        },
    );
    assert_eq!(
        retained_symbols.result.kind,
        NativeActionResultKind::ExecBridgeSymbolsClearFailed
    );
    assert!(retained_symbols.quarantine);

    let symbols_and_undo = classify_execution_finalization(
        result(NativeActionResultKind::ExecBridgeSymbolsClearFailed),
        ExecFinalizationObservation {
            undo_group_may_be_open: true,
            bridge_symbols_may_be_retained: true,
            ..ExecFinalizationObservation::default()
        },
    );
    assert_eq!(
        symbols_and_undo.result.kind,
        NativeActionResultKind::ExecBridgeFinalizationFailed
    );
    assert!(symbols_and_undo.quarantine);
}

#[test]
fn preserves_native_guard_outcomes_as_types() {
    assert_eq!(
        interpret(
            result(NativeActionResultKind::DocGone),
            &Operation::Save {
                id: "D0C0".parse().unwrap(),
            },
        ),
        Err(Error::DocGone)
    );
    assert_eq!(
        interpret(
            result(NativeActionResultKind::DocGenerationChanged),
            &Operation::Save {
                id: "D0C0".parse().unwrap(),
            },
        ),
        Err(Error::DocGenerationChanged)
    );
    assert_eq!(
        interpret(
            result(NativeActionResultKind::ReadOnly),
            &Operation::Save {
                id: "D0C0".parse().unwrap(),
            },
        ),
        Err(Error::ReadOnly("D0C0".parse().unwrap()))
    );
}

#[tokio::test]
async fn dropped_waiter_does_not_cancel_or_release_an_operation() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;

    let first = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let save_action = take_native_action();
    assert_eq!(save_action.kind, NativeActionKind::Save);

    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let second = tokio::spawn(close(id, true));
    tokio::task::yield_now().await;
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
    assert!(try_claim_native_action_wake());

    let close_action = take_native_action();
    assert_eq!(close_action.kind, NativeActionKind::Close);
    replace_document_snapshot(Vec::new());
    complete_native_action(close_action.job_id, result(NativeActionResultKind::Success));
    assert!(second.await.unwrap().is_ok());
    stop();
}

#[tokio::test]
async fn drawing_history_actions_share_the_fifo_and_exact_generation() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;

    let undo_waiter = tokio::spawn(undo(id));
    tokio::task::yield_now().await;
    let undo_action = take_native_action();
    assert_eq!(undo_action.kind, NativeActionKind::Undo);
    assert_eq!(undo_action.document_token, 1);
    assert_eq!(undo_action.database_token, 101);

    let redo_waiter = tokio::spawn(redo(id));
    tokio::task::yield_now().await;
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    replace_document_snapshot(vec![document(1, 101, true)]);
    complete_native_action(undo_action.job_id, result(NativeActionResultKind::Success));
    assert!(undo_waiter.await.unwrap().unwrap().modified);

    let redo_action = take_native_action();
    assert_eq!(redo_action.kind, NativeActionKind::Redo);
    assert_eq!(redo_action.document_token, 1);
    assert_eq!(redo_action.database_token, 101);
    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(redo_action.job_id, result(NativeActionResultKind::Success));
    assert!(!redo_waiter.await.unwrap().unwrap().modified);
    stop();
}

#[tokio::test]
async fn drawing_history_fails_closed_on_missing_or_replaced_documents() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;

    assert_eq!(
        undo("DEAD".parse().unwrap()).await,
        Err(Error::DocNotFound("DEAD".parse().unwrap()))
    );

    let waiter = tokio::spawn(redo(id));
    tokio::task::yield_now().await;
    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::Redo);
    replace_document_snapshot(vec![document(1, 201, false)]);
    complete_native_action(action.job_id, result(NativeActionResultKind::Success));
    assert_eq!(waiter.await.unwrap(), Err(Error::DocGenerationChanged));
    stop();
}

#[tokio::test]
async fn wake_failure_completes_every_job_waiting_on_that_wake() {
    let _test = TEST_LOCK.lock().await;
    reset(Vec::new());

    let first = tokio::spawn(open(drawing_path("first")));
    let second = tokio::spawn(open(drawing_path("second")));
    let third = tokio::spawn(open(drawing_path("third")));
    tokio::task::yield_now().await;

    wake_failed(42);

    for job in [first, second, third] {
        assert_eq!(job.await.unwrap(), Err(Error::ScheduleFailed(42)));
    }

    assert_eq!(take_native_action().kind, NativeActionKind::None);
    stop();
}

#[tokio::test]
async fn shutdown_rejects_pending_work_but_preserves_the_active_operation() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;

    let active = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let save_action = take_native_action();
    assert_eq!(save_action.kind, NativeActionKind::Save);

    let pending = tokio::spawn(close(id, true));
    tokio::task::yield_now().await;
    stop();

    assert_eq!(pending.await.unwrap(), Err(Error::PluginStopping));
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
    assert!(active.await.unwrap().is_ok());
}

#[tokio::test]
async fn routes_the_eval_value_only_after_commit_and_only_once() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) =
        Exec::new(ExecMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    let form = take_execution_step(action.job_id);
    assert_eq!(form.kind(), crate::exec::ExecStepKind::EvaluateForm);
    assert!(form.retain_value());
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CommitUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EmitEvalValue
    );

    assert!(!begin_eval_value(action.job_id + 1, 1, 101).active());
    assert!(!begin_eval_value(action.job_id, 2, 101).active());
    assert!(!begin_eval_value(action.job_id, 1, 202).active());
    let mut writer = begin_eval_value(action.job_id, 1, 101);
    assert!(writer.active());
    assert!(!begin_eval_value(action.job_id, 1, 101).active());
    assert_eq!(writer.write(ValueEvent::Integer(12)), WriteResult::Continue);
    assert_eq!(writer.finish(), WriteResult::Continue);
    assert!(!begin_eval_value(action.job_id, 1, 101).active());

    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(action.job_id, result(NativeActionResultKind::Success));

    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Success);
    let mut rendered = String::new();

    while let Some(chunk) = output.next_chunk().await {
        rendered.push_str(&chunk);
    }

    assert_eq!(rendered, "12\n");
    stop();
}

#[tokio::test]
async fn queued_cancellation_removes_only_that_execution() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;

    let active = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let save_action = take_native_action();
    assert_eq!(save_action.kind, NativeActionKind::Save);

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;
    let job_id = SCHEDULER
        .lock()
        .unwrap()
        .pending
        .front()
        .expect("execution is queued behind save")
        .job_id;

    assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Cancelled);
    assert_eq!(output.next_chunk().await, None);

    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
    assert!(active.await.unwrap().is_ok());
    assert!(!try_claim_native_action_wake());
    stop();
}

#[tokio::test]
async fn dropping_a_queued_execution_waiter_keeps_the_job_and_output_alive() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;

    let active = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let save_action = take_native_action();
    assert_eq!(save_action.kind, NativeActionKind::Save);

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, queued) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;
    let job_id = SCHEDULER
        .lock()
        .unwrap()
        .pending
        .front()
        .expect("execution is queued behind save")
        .job_id;

    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), output.next_chunk())
            .await
            .is_err()
    );
    assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
    assert_eq!(output.next_chunk().await, None);

    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
    assert!(active.await.unwrap().is_ok());
    assert!(!try_claim_native_action_wake());
    stop();
}

#[tokio::test]
async fn wake_failure_stops_a_pending_execution_output_stream() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    wake_failed(42);

    assert_eq!(pending.await.unwrap(), Err(Error::ScheduleFailed(42)));
    assert_eq!(output.next_chunk().await, None);
    assert_eq!(take_native_action().kind, NativeActionKind::None);
    stop();
}

#[tokio::test]
async fn active_cancellation_rolls_back_after_the_current_form() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::QueueExecDriver);
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EvaluateForm
    );

    assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
    assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
    assert_eq!(output.next_chunk().await, None);
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::RollbackUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );

    complete_native_action(action.job_id, result(NativeActionResultKind::Success));
    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Cancelled);
    stop();
}

#[tokio::test]
async fn active_cancellation_before_the_first_form_closes_the_empty_undo_group() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
    assert_eq!(output.next_chunk().await, None);
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CloseEmptyUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );

    complete_native_action(action.job_id, result(NativeActionResultKind::Success));
    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Cancelled);
    stop();
}

#[tokio::test]
async fn cancellation_after_commit_handoff_does_not_cancel_output() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EvaluateForm
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CommitUndoGroup
    );

    assert_eq!(cancel_execution(action.job_id), CancelResult::TooLate);
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(action.job_id, result(NativeActionResultKind::Success));

    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Success);
    assert_eq!(output.next_chunk().await, None);
    stop();
}

#[tokio::test]
async fn shutdown_wakes_output_and_cancels_an_active_execution_safely() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EvaluateForm
    );

    stop();
    assert_eq!(output.next_chunk().await, None);
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::RollbackUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(action.job_id, result(NativeActionResultKind::Success));
    assert_eq!(pending.await.unwrap().unwrap(), ExecOutcome::Cancelled);
}

#[tokio::test]
async fn dropped_execution_waiter_does_not_release_the_active_mutation_job() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let (mut output, executing) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::QueueExecDriver);
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    let form = take_execution_step(action.job_id);
    assert_eq!(form.source(), "form");

    executing.abort();
    assert!(executing.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), output.next_chunk())
            .await
            .is_err()
    );
    let later = tokio::spawn(close(id, true));
    tokio::task::yield_now().await;
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CommitUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(action.job_id, result(NativeActionResultKind::Success));
    assert_eq!(output.next_chunk().await, None);

    assert!(try_claim_native_action_wake());
    let close_action = take_native_action();
    assert_eq!(close_action.kind, NativeActionKind::Close);
    replace_document_snapshot(Vec::new());
    complete_native_action(close_action.job_id, result(NativeActionResultKind::Success));
    assert!(later.await.unwrap().is_ok());
    stop();
}

#[tokio::test]
async fn document_context_restore_failure_amends_a_terminal_execution_outcome() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "ok".into()).unwrap();
    let (_output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::QueueExecDriver);
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EvaluateForm
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CommitUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );

    let blocked = tokio::spawn(save(id));
    tokio::task::yield_now().await;

    complete_native_action(
        action.job_id,
        NativeActionResult {
            kind: NativeActionResultKind::DocContextRestoreFailed,
            native_status: 42,
            native_detail: "unlock failed".into(),
        },
    );

    assert_eq!(
        pending.await.unwrap().unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            message: "unlock failed".into(),
            form_index: None,
            location: None,
            drawing_outcome: crate::exec::DrawingOutcome::Unknown,
        })
    );
    assert_eq!(
        blocked.await.unwrap(),
        Err(Error::NativeMutationStateUnknown)
    );
    assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
    assert_eq!(take_native_action().kind, NativeActionKind::None);
    stop();
}

#[tokio::test]
async fn start_does_not_clear_native_state_quarantine() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        scheduler.stopping = true;
        scheduler.quarantined = true;
    }

    start();

    {
        let scheduler = SCHEDULER.lock().unwrap();
        assert!(!scheduler.stopping);
        assert!(scheduler.quarantined);
    }

    assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
    reset(Vec::new());
    stop();
}

#[tokio::test]
async fn retained_execution_state_quarantines_without_erasing_commit_evidence() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) =
        Exec::new(ExecMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
    let (_output, pending) = spawn_test_execution(id, execution, output);
    tokio::task::yield_now().await;

    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::QueueExecDriver);
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EvaluateForm
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::CommitUndoGroup
    );
    assert!(complete_execution_step(action.job_id, step_success()));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::EmitEvalValue
    );

    let mut writer = begin_eval_value(action.job_id, 1, 101);
    assert_eq!(writer.write(ValueEvent::Integer(12)), WriteResult::Continue);
    assert_eq!(writer.finish(), WriteResult::Continue);
    assert!(complete_execution_step(
        action.job_id,
        ExecStepResult {
            kind: crate::exec::ExecStepResultKind::NativeError,
            native_status: 42,
            lisp_errno: 0,
            detail: "could not clear the retained AutoLISP value".into(),
            bridge_symbols_clear_status: 0,
        }
    ));
    assert_eq!(
        take_execution_step(action.job_id).kind(),
        crate::exec::ExecStepKind::Done
    );

    let blocked = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    complete_execution_native_action(
        action.job_id,
        NativeActionResult {
            kind: NativeActionResultKind::ExecBridgeSymbolsClearFailed,
            native_status: 42,
            native_detail: "reserved execution bridge state remains".into(),
        },
        ExecFinalizationObservation {
            bridge_symbols_may_be_retained: true,
            terminal_cleanup_failed: true,
            ..ExecFinalizationObservation::default()
        },
    );

    assert_eq!(
        pending.await.unwrap().unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            message: "could not clear the retained AutoLISP value".into(),
            form_index: Some(1),
            location: Some(crate::exec::SourceLocation {
                source_name: "inspect.lsp".into(),
                line: 1,
                column: 1,
            }),
            drawing_outcome: crate::exec::DrawingOutcome::Committed,
        })
    );
    assert_eq!(
        blocked.await.unwrap(),
        Err(Error::NativeMutationStateUnknown)
    );
    assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
    assert_eq!(take_native_action().kind, NativeActionKind::None);
    stop();
}

#[tokio::test]
async fn queued_execution_expires_without_starting_a_form() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;
    let saving = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let save_action = take_native_action();
    assert_eq!(save_action.kind, NativeActionKind::Save);

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (job_id, mut output, completion) = admission.into_parts();
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        let job = scheduler
            .pending
            .iter_mut()
            .find(|job| job.job_id == job_id)
            .unwrap();
        job.start_deadline = Some(Instant::now() - Duration::from_millis(1));
    }

    process_due_timers(Instant::now());

    assert_eq!(output.next_chunk().await, None);
    assert_eq!(
        completion.wait().await.unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            message: "execution did not start within 5 seconds".into(),
            form_index: None,
            location: None,
            drawing_outcome: crate::exec::DrawingOutcome::NotStarted,
        })
    );
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    replace_document_snapshot(vec![document(1, 101, false)]);
    complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
    assert!(saving.await.unwrap().is_ok());
    stop();
}

#[tokio::test]
async fn busy_execution_waits_for_a_readiness_retry_without_spinning() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (job_id, _output, completion) = admission.into_parts();

    let first = take_native_action();
    assert_eq!(first.kind, NativeActionKind::QueueExecDriver);
    complete_native_action(first.job_id, result(NativeActionResultKind::NotQuiescent));
    assert_eq!(take_native_action().kind, NativeActionKind::None);

    process_due_timers(Instant::now() + BUSY_RETRY_MAX);
    let retried = take_native_action();
    assert_eq!(retried.kind, NativeActionKind::QueueExecDriver);
    assert_eq!(retried.job_id, job_id);
    assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
    assert_eq!(
        take_execution_step(job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(job_id, result(NativeActionResultKind::Success));
    assert_eq!(completion.wait().await.unwrap(), ExecOutcome::Cancelled);
    stop();
}

#[tokio::test]
async fn deadline_wins_while_the_busy_probe_is_in_flight() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (job_id, _output, completion) = admission.into_parts();
    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::QueueExecDriver);
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        scheduler.active.as_mut().unwrap().start_deadline =
            Some(Instant::now() - Duration::from_millis(1));
    }

    process_due_timers(Instant::now());
    complete_native_action(job_id, result(NativeActionResultKind::NotQuiescent));

    assert!(matches!(
        completion.wait().await.unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            drawing_outcome: crate::exec::DrawingOutcome::NotStarted,
            ..
        })
    ));
    assert_eq!(take_native_action().kind, NativeActionKind::None);
    stop();
}

#[tokio::test]
async fn deadline_winner_survives_a_native_preflight_failure() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (job_id, _output, completion) = admission.into_parts();
    assert_eq!(take_native_action().kind, NativeActionKind::QueueExecDriver);
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        scheduler.active.as_mut().unwrap().start_deadline =
            Some(Instant::now() - Duration::from_millis(1));
    }

    process_due_timers(Instant::now());
    complete_native_action(job_id, result(NativeActionResultKind::UndoDisabled));

    assert_eq!(
        completion.wait().await.unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            message: "execution did not start within 5 seconds".into(),
            form_index: None,
            location: None,
            drawing_outcome: crate::exec::DrawingOutcome::NotStarted,
        })
    );
    stop();
}

#[tokio::test]
async fn deadline_winner_survives_a_failing_begin_step() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (job_id, _output, completion) = admission.into_parts();
    assert_eq!(take_native_action().kind, NativeActionKind::QueueExecDriver);
    assert_eq!(
        take_execution_step(job_id).kind(),
        crate::exec::ExecStepKind::BeginUndoGroup
    );
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        scheduler.active.as_mut().unwrap().start_deadline =
            Some(Instant::now() - Duration::from_millis(1));
    }

    process_due_timers(Instant::now());
    assert!(complete_execution_step(
        job_id,
        ExecStepResult {
            kind: crate::exec::ExecStepResultKind::NativeError,
            native_status: 42,
            lisp_errno: 0,
            detail: "undo begin failed".into(),
            bridge_symbols_clear_status: 0,
        }
    ));
    assert_eq!(
        take_execution_step(job_id).kind(),
        crate::exec::ExecStepKind::Done
    );
    complete_native_action(job_id, result(NativeActionResultKind::Success));

    let ExecOutcome::Failure(failure) = completion.wait().await.unwrap() else {
        panic!("the execution start deadline must remain the terminal cause");
    };

    assert!(
        failure
            .message
            .starts_with("execution did not start within 5 seconds")
    );
    assert!(failure.message.contains("undo begin failed"));
    assert_eq!(
        failure.drawing_outcome,
        crate::exec::DrawingOutcome::NotStarted
    );
    stop();
}

#[tokio::test]
async fn execution_count_capacity_is_released_by_queued_cancellation() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let mut admissions = Vec::new();

    for _ in 0..MAX_ADMITTED_EXECUTIONS {
        let (execution, output) =
            Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        admissions.push(admit_test_execution(id, execution, output).unwrap());
    }

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    assert!(matches!(
        admit_test_execution(id, execution, output),
        Err(Error::ExecCapacity)
    ));

    for admission in admissions {
        let (job_id, _output, completion) = admission.into_parts();
        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(completion.wait().await.unwrap(), ExecOutcome::Cancelled);
    }

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let replacement = admit_test_execution(id, execution, output).unwrap();
    let (job_id, _output, completion) = replacement.into_parts();
    assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
    assert_eq!(completion.wait().await.unwrap(), ExecOutcome::Cancelled);
    stop();
}

#[tokio::test]
async fn detached_execution_retains_its_shared_admission_reservation() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;
    let response_reservation = try_reserve_execution().unwrap();
    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_execution(id, execution, output, response_reservation.clone()).unwrap();
    let (job_id, output, completion) = admission.into_parts();
    drop(output);
    drop(completion);
    drop(response_reservation);

    let other_reservations = (1..MAX_ADMITTED_EXECUTIONS)
        .map(|_| try_reserve_execution().unwrap())
        .collect::<Vec<_>>();
    assert!(try_reserve_execution().is_none());

    assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
    let replacement = try_reserve_execution().unwrap();
    drop(replacement);
    drop(other_reservations);
    stop();
}

#[tokio::test]
async fn queued_cancel_and_deadline_have_one_serialized_winner() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, false)]);
    let id = list().unwrap()[0].id;

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (expired_id, _output, expired_completion) = admission.into_parts();
    {
        let mut scheduler = SCHEDULER.lock().unwrap();
        scheduler
            .pending
            .iter_mut()
            .find(|job| job.job_id == expired_id)
            .unwrap()
            .start_deadline = Some(Instant::now() - Duration::from_millis(1));
    }

    process_due_timers(Instant::now());
    assert_eq!(cancel_execution(expired_id), CancelResult::NotFound);
    assert!(matches!(
        expired_completion.wait().await.unwrap(),
        ExecOutcome::Failure(crate::exec::ExecFailure {
            drawing_outcome: crate::exec::DrawingOutcome::NotStarted,
            ..
        })
    ));

    let (execution, output) = Exec::new(ExecMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (cancelled_id, _output, cancelled_completion) = admission.into_parts();
    assert_eq!(cancel_execution(cancelled_id), CancelResult::Accepted);
    process_due_timers(Instant::now() + EXECUTION_START_TIMEOUT);
    assert_eq!(
        cancelled_completion.wait().await.unwrap(),
        ExecOutcome::Cancelled
    );
    stop();
}

#[tokio::test]
async fn mutation_job_capacity_bounds_disconnected_waiters() {
    let _test = TEST_LOCK.lock().await;
    reset(vec![document(1, 101, true)]);
    let id = list().unwrap()[0].id;
    let active = tokio::spawn(save(id));
    tokio::task::yield_now().await;
    let action = take_native_action();
    assert_eq!(action.kind, NativeActionKind::Save);

    let mut queued = Vec::new();

    for _ in 1..MAX_MUTATION_JOBS {
        queued.push(tokio::spawn(save(id)));
        tokio::task::yield_now().await;
    }

    assert_eq!(save(id).await, Err(Error::MutationCapacity));

    stop();
    complete_native_action(action.job_id, result(NativeActionResultKind::SaveFailed));
    assert!(matches!(active.await.unwrap(), Err(Error::SaveFailed(_))));

    for waiter in queued {
        assert_eq!(waiter.await.unwrap(), Err(Error::PluginStopping));
    }
}

fn admit_test_execution(
    id: DocId,
    execution: Exec,
    output: OutputStream,
) -> Result<ExecAdmission, Error> {
    let reservation = try_reserve_execution().ok_or(Error::ExecCapacity)?;
    admit_execution(id, execution, output, reservation)
}

fn spawn_test_execution(
    id: DocId,
    execution: Exec,
    output: OutputStream,
) -> (
    OutputStream,
    tokio::task::JoinHandle<Result<ExecOutcome, Error>>,
) {
    let admission = admit_test_execution(id, execution, output).unwrap();
    let (_, output, completion) = admission.into_parts();
    (output, tokio::spawn(completion.wait()))
}

fn result(kind: NativeActionResultKind) -> NativeActionResult {
    NativeActionResult {
        kind,
        native_status: 0,
        native_detail: String::new(),
    }
}

fn step_success() -> ExecStepResult {
    ExecStepResult {
        kind: crate::exec::ExecStepResultKind::Success,
        native_status: 0,
        lisp_errno: 0,
        detail: String::new(),
        bridge_symbols_clear_status: 0,
    }
}

fn document(
    document_token: usize,
    database_token: usize,
    modified: bool,
) -> crate::ffi::NativeDocSnapshot {
    crate::ffi::NativeDocSnapshot {
        document_token,
        database_token,
        name: "/tmp/house.dwg".into(),
        named: true,
        modified,
        read_only: false,
    }
}

fn drawing_path(name: &str) -> DrawingPath {
    let path = std::env::temp_dir().join(format!(
        "acadctl-scheduler-{}-{name}.dwg",
        std::process::id()
    ));
    std::fs::write(&path, []).unwrap();
    let drawing = DrawingPath::canonicalize(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    drawing
}

fn reset(documents: Vec<crate::ffi::NativeDocSnapshot>) {
    stop();
    SCHEDULER.lock().unwrap().quarantined = false;
    replace_document_snapshot(documents);
    start();
}
