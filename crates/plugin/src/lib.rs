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

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeValueEventKind {
        Invalid,
        BeginList,
        EndList,
        Dot,
        Nil,
        True,
        Integer,
        Real,
        Point2,
        Point3,
        BeginString,
        StringChunk,
        EndString,
        BeginSymbol,
        SymbolChunk,
        EndSymbol,
        Entity,
        SelectionSet,
        VlaObject,
        File,
        Function,
        ErrorObject,
        Void,
        Unsupported,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeValueWriteResult {
        Continue,
        Inactive,
        Disconnected,
        Cancelled,
        Stopped,
        Finished,
        InvalidSequence,
        LimitExceeded,
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

    struct NativeValueEvent {
        kind: NativeValueEventKind,
        integer: i64,
        number: u64,
        native_type: u32,
        real: f64,
        x: f64,
        y: f64,
        z: f64,
        has_payload: bool,
    }

    extern "Rust" {
        type NativeExecutionStep;
        type NativeValueWriter;

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

        fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter>;

        fn value_writer_active(writer: &NativeValueWriter) -> bool;

        fn write_value_event(
            writer: &mut NativeValueWriter,
            event: NativeValueEvent,
            text: &str,
        ) -> NativeValueWriteResult;

        fn finish_value_writer(writer: Box<NativeValueWriter>) -> NativeValueWriteResult;

        fn stop_rpc_server();
    }
}

mod documents;
mod execution;
mod rpc_server;
mod scheduler;

use execution::NativeExecutionStep;
use execution::value_bridge::{NativeValueWriter, ValueEvent, WriteResult};

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

fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter> {
    Box::new(scheduler::begin_println(document_token, database_token))
}

fn value_writer_active(writer: &NativeValueWriter) -> bool {
    writer.active()
}

fn write_value_event(
    writer: &mut NativeValueWriter,
    event: ffi::NativeValueEvent,
    text: &str,
) -> ffi::NativeValueWriteResult {
    let value = match event.kind {
        ffi::NativeValueEventKind::Invalid => ValueEvent::Invalid,
        ffi::NativeValueEventKind::BeginList => ValueEvent::BeginList,
        ffi::NativeValueEventKind::EndList => ValueEvent::EndList,
        ffi::NativeValueEventKind::Dot => ValueEvent::Dot,
        ffi::NativeValueEventKind::Nil => ValueEvent::Nil,
        ffi::NativeValueEventKind::True => ValueEvent::True,
        ffi::NativeValueEventKind::Integer => ValueEvent::Integer(event.integer),
        ffi::NativeValueEventKind::Real => ValueEvent::Real(event.real),
        ffi::NativeValueEventKind::Point2 => ValueEvent::Point2(event.x, event.y),
        ffi::NativeValueEventKind::Point3 => ValueEvent::Point3(event.x, event.y, event.z),
        ffi::NativeValueEventKind::BeginString => ValueEvent::BeginString,
        ffi::NativeValueEventKind::StringChunk => ValueEvent::StringChunk(text),
        ffi::NativeValueEventKind::EndString => ValueEvent::EndString,
        ffi::NativeValueEventKind::BeginSymbol => ValueEvent::BeginSymbol,
        ffi::NativeValueEventKind::SymbolChunk => ValueEvent::SymbolChunk(text),
        ffi::NativeValueEventKind::EndSymbol => ValueEvent::EndSymbol,
        ffi::NativeValueEventKind::Entity => ValueEvent::Entity(event.has_payload.then_some(text)),
        ffi::NativeValueEventKind::SelectionSet => {
            ValueEvent::SelectionSet(event.has_payload.then_some(event.number))
        }
        ffi::NativeValueEventKind::VlaObject => {
            ValueEvent::VlaObject(event.has_payload.then_some(text))
        }
        ffi::NativeValueEventKind::File => ValueEvent::File,
        ffi::NativeValueEventKind::Function => {
            ValueEvent::Function(event.has_payload.then_some(text))
        }
        ffi::NativeValueEventKind::ErrorObject => ValueEvent::ErrorObject,
        ffi::NativeValueEventKind::Void => ValueEvent::Void,
        ffi::NativeValueEventKind::Unsupported => {
            ValueEvent::Unsupported(event.has_payload.then_some(event.native_type))
        }
        _ => ValueEvent::Invalid,
    };
    native_value_write_result(writer.write(value))
}

#[allow(
    clippy::boxed_local,
    reason = "CXX transfers exclusive ownership so finish can validate and close the writer"
)]
fn finish_value_writer(writer: Box<NativeValueWriter>) -> ffi::NativeValueWriteResult {
    native_value_write_result((*writer).finish())
}

fn native_value_write_result(result: WriteResult) -> ffi::NativeValueWriteResult {
    match result {
        WriteResult::Continue => ffi::NativeValueWriteResult::Continue,
        WriteResult::Inactive => ffi::NativeValueWriteResult::Inactive,
        WriteResult::Disconnected => ffi::NativeValueWriteResult::Disconnected,
        WriteResult::Cancelled => ffi::NativeValueWriteResult::Cancelled,
        WriteResult::Stopped => ffi::NativeValueWriteResult::Stopped,
        WriteResult::Finished => ffi::NativeValueWriteResult::Finished,
        WriteResult::InvalidSequence => ffi::NativeValueWriteResult::InvalidSequence,
        WriteResult::LimitExceeded => ffi::NativeValueWriteResult::LimitExceeded,
    }
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
