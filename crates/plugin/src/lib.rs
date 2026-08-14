use std::sync::atomic::{AtomicBool, Ordering};

#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct NativeDocumentState {
        token: usize,
        database_token: usize,
        name: String,
        named: bool,
        modified: bool,
        read_only: bool,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionKind {
        None,
        Open,
        Save,
        Close,
        RunExecution,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionResultKind {
        Success,
        DocumentGone,
        DocumentChanged,
        Unnamed,
        ReadOnly,
        Dirty,
        OpenFailed,
        LockFailed,
        SaveFailed,
        CloseFailed,
        NotQuiescent,
        UndoDisabled,
        ContextFailed,
        ContextCleanupFailed,
        ExecutionLeaseFailed,
        ExecutionBridgeFailed,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecutionStepKind {
        Invalid,
        Begin,
        Form,
        Commit,
        Abort,
        Rollback,
        Done,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecutionStepResultKind {
        Success,
        LispError,
        NativeError,
    }

    struct NativeAction {
        request_id: u64,
        kind: NativeActionKind,
        document_token: usize,
        database_token: usize,
        path: String,
        discard: bool,
    }

    struct NativeActionResult {
        kind: NativeActionResultKind,
        native_status: i32,
        native_detail: String,
    }

    struct NativeExecutionStepResult {
        kind: NativeExecutionStepResultKind,
        native_status: i32,
        lisp_errno: i32,
        detail: String,
    }

    extern "Rust" {
        type NativeExecutionStep;

        fn start_rpc_server() -> String;

        fn mark_documents_dirty();

        fn take_documents_dirty() -> bool;

        fn replace_documents(documents: Vec<NativeDocumentState>);

        fn take_native_action() -> NativeAction;

        fn complete_native_action(request_id: u64, result: NativeActionResult);

        fn native_actions_need_wake() -> bool;

        fn native_action_wake_failed(status: i32);

        fn take_execution_step(execution_id: u64) -> Box<NativeExecutionStep>;

        fn execution_step_kind(step: &NativeExecutionStep) -> NativeExecutionStepKind;

        fn execution_step_source(step: &NativeExecutionStep) -> &str;

        fn execution_evaluator_source() -> &'static str;

        fn complete_execution_step(execution_id: u64, result: NativeExecutionStepResult) -> bool;

        fn abandon_execution(execution_id: u64, result: NativeExecutionStepResult) -> bool;

        fn stop_rpc_server();
    }
}

mod documents;
mod execution;
mod rpc_server;
mod scheduler;

use execution::NativeExecutionStep;

static DOCUMENTS_DIRTY: AtomicBool = AtomicBool::new(false);

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn mark_documents_dirty() {
    DOCUMENTS_DIRTY.store(true, Ordering::Relaxed);
}

fn take_documents_dirty() -> bool {
    DOCUMENTS_DIRTY.swap(false, Ordering::Relaxed)
}

fn replace_documents(documents: Vec<ffi::NativeDocumentState>) {
    rpc_server::replace_documents(documents);
}

fn take_native_action() -> ffi::NativeAction {
    scheduler::take()
}

fn complete_native_action(request_id: u64, result: ffi::NativeActionResult) {
    scheduler::complete(request_id, result);
}

fn native_actions_need_wake() -> bool {
    scheduler::native_actions_need_wake()
}

fn native_action_wake_failed(status: i32) {
    scheduler::wake_failed(status);
}

fn take_execution_step(execution_id: u64) -> Box<NativeExecutionStep> {
    Box::new(scheduler::take_execution_step(execution_id))
}

fn execution_step_kind(step: &NativeExecutionStep) -> ffi::NativeExecutionStepKind {
    match step.kind() {
        execution::StepKind::Invalid => ffi::NativeExecutionStepKind::Invalid,
        execution::StepKind::Begin => ffi::NativeExecutionStepKind::Begin,
        execution::StepKind::Form => ffi::NativeExecutionStepKind::Form,
        execution::StepKind::Commit => ffi::NativeExecutionStepKind::Commit,
        execution::StepKind::Abort => ffi::NativeExecutionStepKind::Abort,
        execution::StepKind::Rollback => ffi::NativeExecutionStepKind::Rollback,
        execution::StepKind::Done => ffi::NativeExecutionStepKind::Done,
    }
}

fn execution_step_source(step: &NativeExecutionStep) -> &str {
    step.source()
}

fn execution_evaluator_source() -> &'static str {
    execution::EVALUATOR_SOURCE
}

fn complete_execution_step(execution_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::complete_execution_step(execution_id, execution_step_result(result))
}

fn abandon_execution(execution_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::abandon_execution(execution_id, execution_step_result(result))
}

fn execution_step_result(result: ffi::NativeExecutionStepResult) -> execution::StepResult {
    let kind = match result.kind {
        ffi::NativeExecutionStepResultKind::Success => execution::StepResultKind::Success,
        ffi::NativeExecutionStepResultKind::LispError => execution::StepResultKind::LispError,
        ffi::NativeExecutionStepResultKind::NativeError => execution::StepResultKind::NativeError,
        _ => execution::StepResultKind::NativeError,
    };
    execution::StepResult {
        kind,
        native_status: result.native_status,
        lisp_errno: result.lisp_errno,
        detail: result.detail,
    }
}

fn stop_rpc_server() {
    rpc_server::stop();
}
