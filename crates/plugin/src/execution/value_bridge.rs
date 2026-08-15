use super::output::EmitResult;
use super::value::{PrintError, PrintMode, ValuePrinter};
use super::{ValueBridgeFailure, ValueOutputLease};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    Continue,
    Inactive,
    Disconnected,
    Cancelled,
    Stopped,
    Finished,
    InvalidSequence,
    LimitExceeded,
}

pub enum ValueEvent<'a> {
    Invalid,
    BeginList,
    EndList,
    Dot,
    Nil,
    True,
    Integer(i64),
    Real(f64),
    Point2(f64, f64),
    Point3(f64, f64, f64),
    BeginString,
    StringChunk(&'a str),
    EndString,
    BeginSymbol,
    SymbolChunk(&'a str),
    EndSymbol,
    Entity(Option<&'a str>),
    SelectionSet(Option<u64>),
    VlaObject(Option<&'a str>),
    File,
    Function(Option<&'a str>),
    ErrorObject,
    Void,
    Unsupported(Option<u32>),
    Object(Option<&'a str>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriterPolicy {
    Inactive,
    Println,
    EvalValue,
}

pub struct NativeValueWriter {
    printer: Option<ValuePrinter>,
    lease: Option<ValueOutputLease>,
    policy: WriterPolicy,
    result: WriteResult,
}

impl NativeValueWriter {
    pub fn inactive() -> Self {
        Self {
            printer: None,
            lease: None,
            policy: WriterPolicy::Inactive,
            result: WriteResult::Inactive,
        }
    }

    pub(crate) fn println(lease: ValueOutputLease) -> Self {
        Self::new(lease, PrintMode::Display, WriterPolicy::Println)
    }

    pub(crate) fn eval_value(lease: ValueOutputLease) -> Self {
        Self::new(lease, PrintMode::Readable, WriterPolicy::EvalValue)
    }

    fn new(lease: ValueOutputLease, mode: PrintMode, policy: WriterPolicy) -> Self {
        let sink = lease.output_sink();
        let status = sink.emit("");
        let result = emit_result(status);
        let mut writer = Self {
            printer: (status == EmitResult::Written).then(|| ValuePrinter::new(sink, mode)),
            lease: Some(lease),
            policy,
            result,
        };
        if status != EmitResult::Written {
            let failure = output_failure(policy, status);
            writer.retire(failure, result);
        }
        writer
    }

    pub fn active(&self) -> bool {
        self.result == WriteResult::Continue
            && self.lease.as_ref().is_some_and(ValueOutputLease::is_open)
    }

    pub fn write(&mut self, event: ValueEvent<'_>) -> WriteResult {
        if self.result != WriteResult::Continue {
            return self.result;
        }
        if !self.lease.as_ref().is_some_and(ValueOutputLease::is_open) {
            self.retire(None, WriteResult::Inactive);
            return self.result;
        }
        let Some(printer) = self.printer.as_mut() else {
            return self.fail(
                ValueBridgeFailure::InvalidSequence,
                WriteResult::InvalidSequence,
            );
        };
        let result = match event {
            ValueEvent::Invalid => Err(PrintError::InvalidSequence),
            ValueEvent::BeginList => printer.begin_list(),
            ValueEvent::EndList => printer.end_list(),
            ValueEvent::Dot => printer.dot(),
            ValueEvent::Nil => printer.nil(),
            ValueEvent::True => printer.true_value(),
            ValueEvent::Integer(value) => printer.integer(value),
            ValueEvent::Real(value) => printer.real(value),
            ValueEvent::Point2(x, y) => printer.point(&[x, y]),
            ValueEvent::Point3(x, y, z) => printer.point(&[x, y, z]),
            ValueEvent::BeginString => printer.begin_string(),
            ValueEvent::StringChunk(text) => printer.string_chunk(text),
            ValueEvent::EndString => printer.end_string(),
            ValueEvent::BeginSymbol => printer.begin_symbol(),
            ValueEvent::SymbolChunk(text) => printer.symbol_chunk(text),
            ValueEvent::EndSymbol => printer.end_symbol(),
            ValueEvent::Entity(handle) => printer.entity(handle),
            ValueEvent::SelectionSet(number) => printer.selection_set(number),
            ValueEvent::VlaObject(class_name) => printer.vla_object(class_name),
            ValueEvent::File => printer.file(),
            ValueEvent::Function(name) => printer.function(name),
            ValueEvent::ErrorObject => printer.error_object(),
            ValueEvent::Void => printer.void(),
            ValueEvent::Unsupported(native_type) => printer.unsupported(native_type),
            ValueEvent::Object(label) => printer.object(label),
        };
        self.handle(result)
    }

    pub fn finish(mut self) -> WriteResult {
        if self.result != WriteResult::Continue {
            return self.result;
        }
        if !self.lease.as_ref().is_some_and(ValueOutputLease::is_open) {
            self.retire(None, WriteResult::Inactive);
            return self.result;
        }
        let Some(printer) = self.printer.take() else {
            return self.fail(
                ValueBridgeFailure::InvalidSequence,
                WriteResult::InvalidSequence,
            );
        };
        if self.policy != WriterPolicy::Inactive && printer.root_values() != 1 {
            return self.fail(
                ValueBridgeFailure::InvalidSequence,
                WriteResult::InvalidSequence,
            );
        }
        match printer.finish() {
            Ok(()) => {
                self.retire(None, WriteResult::Continue);
                self.result
            }
            result => self.handle(result),
        }
    }

    fn handle(&mut self, result: Result<(), PrintError>) -> WriteResult {
        match result {
            Ok(()) => WriteResult::Continue,
            Err(PrintError::InvalidSequence) => self.fail(
                ValueBridgeFailure::InvalidSequence,
                WriteResult::InvalidSequence,
            ),
            Err(PrintError::LimitExceeded) => self.fail(
                ValueBridgeFailure::LimitExceeded,
                WriteResult::LimitExceeded,
            ),
            Err(PrintError::Output(result)) => {
                self.retire(output_failure(self.policy, result), emit_result(result));
                self.result
            }
        }
    }

    fn fail(&mut self, failure: ValueBridgeFailure, result: WriteResult) -> WriteResult {
        self.retire(Some(failure), result);
        self.result
    }

    fn retire(&mut self, failure: Option<ValueBridgeFailure>, result: WriteResult) {
        self.printer = None;
        if let Some(lease) = self.lease.take() {
            lease.release(failure);
        }
        self.result = result;
    }
}

const fn emit_result(result: EmitResult) -> WriteResult {
    match result {
        EmitResult::Written => WriteResult::Continue,
        EmitResult::Disconnected => WriteResult::Disconnected,
        EmitResult::Cancelled => WriteResult::Cancelled,
        EmitResult::Stopped => WriteResult::Stopped,
        EmitResult::Finished => WriteResult::Finished,
    }
}

const fn output_failure(policy: WriterPolicy, result: EmitResult) -> Option<ValueBridgeFailure> {
    match (policy, result) {
        (_, EmitResult::Finished) => Some(ValueBridgeFailure::OutputFinished),
        (WriterPolicy::EvalValue, EmitResult::Cancelled) => {
            Some(ValueBridgeFailure::PostCommitCancelled)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::output::{OutputStream, channel};
    use super::super::{ExecutionIo, ValueBridgeState};
    use super::*;

    #[tokio::test]
    async fn prints_exactly_one_display_value() {
        let (io, mut stream) = execution_io();
        let terminal = io.output_sink();
        let mut writer = NativeValueWriter::println(println_lease(&io));

        assert_eq!(writer.write(ValueEvent::BeginString), WriteResult::Continue);
        assert_eq!(
            writer.write(ValueEvent::StringChunk("created: ")),
            WriteResult::Continue
        );
        assert_eq!(writer.write(ValueEvent::EndString), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::Println),
            None
        );
        terminal.finish();

        assert_eq!(collect(&mut stream).await, "created: \n");
    }

    #[test]
    fn println_requires_exactly_one_root() {
        let (empty_io, _stream) = execution_io();
        let empty = NativeValueWriter::println(println_lease(&empty_io));
        assert_eq!(empty.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            empty_io.close_value_output(super::super::ValueOutputKind::Println),
            Some(ValueBridgeFailure::InvalidSequence)
        );

        let (multiple_io, _stream) = execution_io();
        let mut multiple = NativeValueWriter::println(println_lease(&multiple_io));
        assert_eq!(
            multiple.write(ValueEvent::Integer(1)),
            WriteResult::Continue
        );
        assert_eq!(
            multiple.write(ValueEvent::Integer(2)),
            WriteResult::Continue
        );
        assert_eq!(multiple.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            multiple_io.close_value_output(super::super::ValueOutputKind::Println),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn eval_value_requires_exactly_one_root() {
        let (io, _stream) = execution_io();
        let writer = NativeValueWriter::eval_value(eval_value_lease(&io));

        assert_eq!(writer.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn eval_value_accepts_one_nested_root_and_rejects_two_roots() {
        let (one_io, _one_stream) = execution_io();
        let mut one = NativeValueWriter::eval_value(eval_value_lease(&one_io));
        assert_eq!(one.write(ValueEvent::BeginList), WriteResult::Continue);
        assert_eq!(one.write(ValueEvent::Integer(1)), WriteResult::Continue);
        assert_eq!(one.write(ValueEvent::EndList), WriteResult::Continue);
        assert_eq!(one.finish(), WriteResult::Continue);
        assert_eq!(
            one_io.close_value_output(super::super::ValueOutputKind::EvalValue),
            None
        );

        let (two_io, _two_stream) = execution_io();
        let mut two = NativeValueWriter::eval_value(eval_value_lease(&two_io));
        assert_eq!(two.write(ValueEvent::Integer(1)), WriteResult::Continue);
        assert_eq!(two.write(ValueEvent::Integer(2)), WriteResult::Continue);
        assert_eq!(two.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            two_io.close_value_output(super::super::ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn eval_value_epoch_requires_exactly_one_writer_claim() {
        let (io, _stream) = execution_io();
        io.begin_value_output(super::super::ValueOutputKind::EvalValue);
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::MissingValue)
        );

        io.begin_value_output(super::super::ValueOutputKind::EvalValue);
        let mut writer = NativeValueWriter::eval_value(
            io.acquire_value_output(super::super::ValueOutputKind::EvalValue)
                .expect("first claim succeeds"),
        );
        assert!(
            io.acquire_value_output(super::super::ValueOutputKind::EvalValue)
                .is_none()
        );
        assert_eq!(writer.write(ValueEvent::Nil), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert!(
            io.acquire_value_output(super::super::ValueOutputKind::EvalValue)
                .is_none()
        );
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::EvalValue),
            None
        );
    }

    #[test]
    fn disconnected_eval_writer_still_satisfies_the_required_claim() {
        let (io, stream) = execution_io();
        let lease = eval_value_lease(&io);
        drop(stream);

        let writer = NativeValueWriter::eval_value(lease);
        assert_eq!(writer.result, WriteResult::Disconnected);
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::EvalValue),
            None
        );
    }

    #[test]
    fn impossible_post_commit_cancellation_is_classified_in_rust() {
        let (io, _stream) = execution_io();
        let lease = eval_value_lease(&io);
        io.output.request_cancel();

        let writer = NativeValueWriter::eval_value(lease);
        assert_eq!(writer.result, WriteResult::Cancelled);
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::PostCommitCancelled)
        );
    }

    #[test]
    fn dropping_an_active_writer_records_an_abandoned_bridge() {
        let (io, _stream) = execution_io();
        drop(NativeValueWriter::println(println_lease(&io)));

        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::Println),
            Some(ValueBridgeFailure::Abandoned)
        );
    }

    #[test]
    fn terminal_writer_releases_its_formatter_and_execution_io() {
        let (io, stream) = execution_io();
        let lease = println_lease(&io);
        drop(stream);

        let writer = NativeValueWriter::println(lease);
        assert_eq!(writer.result, WriteResult::Disconnected);
        assert!(writer.printer.is_none());
        assert!(writer.lease.is_none());
        assert_eq!(
            io.close_value_output(super::super::ValueOutputKind::Println),
            None
        );
    }

    fn execution_io() -> (Arc<ExecutionIo>, OutputStream) {
        let (output, stream) = channel();
        (
            Arc::new(ExecutionIo {
                output,
                bridge: Mutex::new(ValueBridgeState::default()),
            }),
            stream,
        )
    }

    fn println_lease(io: &Arc<ExecutionIo>) -> ValueOutputLease {
        io.begin_value_output(super::super::ValueOutputKind::Println);
        io.acquire_value_output(super::super::ValueOutputKind::Println)
            .expect("println output is open")
    }

    fn eval_value_lease(io: &Arc<ExecutionIo>) -> ValueOutputLease {
        io.begin_value_output(super::super::ValueOutputKind::EvalValue);
        io.acquire_value_output(super::super::ValueOutputKind::EvalValue)
            .expect("eval value output is open")
    }

    async fn collect(stream: &mut OutputStream) -> String {
        let mut output = String::new();
        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }
        output
    }
}
