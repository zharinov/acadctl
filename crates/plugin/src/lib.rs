#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct NativeDocumentSnapshot {
        document_token: usize,
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
        Undo,
        Redo,
        QueueExecutionDriver,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionResultKind {
        Success,
        DocumentGone,
        DocumentGenerationChanged,
        Unnamed,
        ReadOnly,
        Dirty,
        OpenFailed,
        LockFailed,
        SaveFailed,
        CloseFailed,
        HistoryFailed,
        NotQuiescent,
        UndoDisabled,
        DocumentContextFailed,
        DocumentContextRestoreFailed,
        ExecutionBridgeFinalizationFailed,
        EvaluatorSymbolsClearFailed,
        ExecutionBridgeFailed,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecutionStepKind {
        Invalid,
        BeginUndoGroup,
        EvaluateForm,
        CommitUndoGroup,
        EmitEvalValue,
        ClearRetainedEvalValue,
        CloseEmptyUndoGroup,
        RollbackUndoGroup,
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
        Object,
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

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeLispPayloadKind {
        Invalid,
        Nil,
        Integer,
        Real,
        String,
        Entity,
    }

    struct NativeAction {
        job_id: u64,
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
        evaluator_symbols_clear_status: i32,
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

    struct NativeLispValueEvent {
        code: i32,
        payload_kind: NativeLispPayloadKind,
        integer: i64,
        real: f64,
        has_text: bool,
    }

    extern "Rust" {
        type NativeExecutionStep;
        type NativeValueWriter;

        fn start_rpc_server() -> String;

        fn publish_document_snapshot(documents: Vec<NativeDocumentSnapshot>);

        fn take_native_action() -> NativeAction;

        fn complete_native_action(job_id: u64, result: NativeActionResult);

        fn native_actions_need_wake() -> bool;

        fn native_state_may_be_ready();

        fn native_action_wake_failed(status: i32);

        fn take_execution_step(job_id: u64) -> Box<NativeExecutionStep>;

        fn execution_step_kind(step: &NativeExecutionStep) -> NativeExecutionStepKind;

        fn execution_step_source(step: &NativeExecutionStep) -> &str;

        fn execution_step_retain_value(step: &NativeExecutionStep) -> bool;

        fn form_evaluator_source() -> &'static str;

        fn eval_value_visitor_source() -> &'static str;

        fn native_diagnostic_capture_units() -> usize;

        fn complete_execution_step(job_id: u64, result: NativeExecutionStepResult) -> bool;

        fn abandon_execution(job_id: u64, result: NativeExecutionStepResult) -> bool;

        fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter>;

        fn begin_eval_value(
            job_id: u64,
            document_token: usize,
            database_token: usize,
        ) -> Box<NativeValueWriter>;

        fn value_writer_active(writer: &NativeValueWriter) -> bool;

        fn write_value_event(
            writer: &mut NativeValueWriter,
            event: NativeValueEvent,
            text: &str,
        ) -> NativeValueWriteResult;

        fn write_lisp_value_event(
            writer: &mut NativeValueWriter,
            event: NativeLispValueEvent,
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

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn publish_document_snapshot(documents: Vec<ffi::NativeDocumentSnapshot>) {
    scheduler::replace_document_snapshot(documents);
}

fn take_native_action() -> ffi::NativeAction {
    scheduler::take_native_action()
}

fn complete_native_action(job_id: u64, result: ffi::NativeActionResult) {
    scheduler::complete_native_action(job_id, result);
}

fn native_actions_need_wake() -> bool {
    scheduler::native_actions_need_wake()
}

fn native_state_may_be_ready() {
    scheduler::native_state_may_be_ready();
}

fn native_action_wake_failed(status: i32) {
    scheduler::wake_failed(status);
}

fn take_execution_step(job_id: u64) -> Box<NativeExecutionStep> {
    Box::new(scheduler::take_execution_step(job_id))
}

fn execution_step_kind(step: &NativeExecutionStep) -> ffi::NativeExecutionStepKind {
    match step.kind() {
        execution::ExecutionStepKind::Invalid => ffi::NativeExecutionStepKind::Invalid,
        execution::ExecutionStepKind::BeginUndoGroup => {
            ffi::NativeExecutionStepKind::BeginUndoGroup
        }
        execution::ExecutionStepKind::EvaluateForm => ffi::NativeExecutionStepKind::EvaluateForm,
        execution::ExecutionStepKind::CommitUndoGroup => {
            ffi::NativeExecutionStepKind::CommitUndoGroup
        }
        execution::ExecutionStepKind::EmitEvalValue => ffi::NativeExecutionStepKind::EmitEvalValue,
        execution::ExecutionStepKind::ClearRetainedEvalValue => {
            ffi::NativeExecutionStepKind::ClearRetainedEvalValue
        }
        execution::ExecutionStepKind::CloseEmptyUndoGroup => {
            ffi::NativeExecutionStepKind::CloseEmptyUndoGroup
        }
        execution::ExecutionStepKind::RollbackUndoGroup => {
            ffi::NativeExecutionStepKind::RollbackUndoGroup
        }
        execution::ExecutionStepKind::Done => ffi::NativeExecutionStepKind::Done,
    }
}

fn execution_step_source(step: &NativeExecutionStep) -> &str {
    step.source()
}

fn execution_step_retain_value(step: &NativeExecutionStep) -> bool {
    step.retain_value()
}

fn form_evaluator_source() -> &'static str {
    execution::FORM_EVALUATOR_SOURCE
}

fn eval_value_visitor_source() -> &'static str {
    execution::visitor::source()
}

fn native_diagnostic_capture_units() -> usize {
    acadctl_rpc::MAX_DIAGNOSTIC_BYTES + 1
}

fn complete_execution_step(job_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::complete_execution_step(job_id, execution_step_result(result))
}

fn abandon_execution(job_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::abandon_execution(job_id, execution_step_result(result))
}

fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter> {
    Box::new(scheduler::begin_println(document_token, database_token))
}

fn begin_eval_value(
    job_id: u64,
    document_token: usize,
    database_token: usize,
) -> Box<NativeValueWriter> {
    Box::new(scheduler::begin_eval_value(
        job_id,
        document_token,
        database_token,
    ))
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
        ffi::NativeValueEventKind::Object => ValueEvent::Object(event.has_payload.then_some(text)),
        _ => ValueEvent::Invalid,
    };
    native_value_write_result(writer.write(value))
}

fn write_lisp_value_event(
    writer: &mut NativeValueWriter,
    event: ffi::NativeLispValueEvent,
    text: &str,
) -> ffi::NativeValueWriteResult {
    use execution::visitor::Payload;

    let payload = match event.payload_kind {
        ffi::NativeLispPayloadKind::Nil => Payload::Nil,
        ffi::NativeLispPayloadKind::Integer => Payload::Integer(event.integer),
        ffi::NativeLispPayloadKind::Real => Payload::Real(event.real),
        ffi::NativeLispPayloadKind::String => Payload::String(text),
        ffi::NativeLispPayloadKind::Entity => Payload::Entity(event.has_text.then_some(text)),
        _ => Payload::Invalid,
    };
    let value = execution::visitor::value_event(event.code, payload);
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

fn execution_step_result(result: ffi::NativeExecutionStepResult) -> execution::ExecutionStepResult {
    let kind = match result.kind {
        ffi::NativeExecutionStepResultKind::Success => execution::ExecutionStepResultKind::Success,
        ffi::NativeExecutionStepResultKind::LispError => {
            execution::ExecutionStepResultKind::LispError
        }
        ffi::NativeExecutionStepResultKind::NativeError => {
            execution::ExecutionStepResultKind::NativeError
        }
        _ => execution::ExecutionStepResultKind::NativeError,
    };
    execution::ExecutionStepResult {
        kind,
        native_status: result.native_status,
        lisp_errno: result.lisp_errno,
        detail: result.detail,
        evaluator_symbols_clear_status: result.evaluator_symbols_clear_status,
    }
}

fn stop_rpc_server() {
    rpc_server::stop();
}
