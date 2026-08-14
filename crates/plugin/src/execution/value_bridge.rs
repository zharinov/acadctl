use super::output::EmitResult;
use super::value::{PrintError, PrintMode, ValuePrinter};
use super::{FormOutputLease, ValueBridgeFailure};

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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RootPolicy {
    Any,
    ExactlyOne,
}

pub struct NativeValueWriter {
    printer: Option<ValuePrinter>,
    lease: Option<FormOutputLease>,
    root_policy: RootPolicy,
    result: WriteResult,
}

impl NativeValueWriter {
    pub fn inactive() -> Self {
        Self {
            printer: None,
            lease: None,
            root_policy: RootPolicy::Any,
            result: WriteResult::Inactive,
        }
    }

    pub(crate) fn println(lease: FormOutputLease) -> Self {
        Self::new(lease, PrintMode::Display, RootPolicy::Any)
    }

    #[cfg(test)]
    fn eval_value(lease: FormOutputLease) -> Self {
        Self::new(lease, PrintMode::Readable, RootPolicy::ExactlyOne)
    }

    fn new(lease: FormOutputLease, mode: PrintMode, root_policy: RootPolicy) -> Self {
        let sink = lease.output_sink();
        let status = sink.emit("");
        let result = emit_result(status);
        let mut writer = Self {
            printer: (status == EmitResult::Written).then(|| ValuePrinter::new(sink, mode)),
            lease: Some(lease),
            root_policy,
            result,
        };
        if status != EmitResult::Written {
            let failure =
                (status == EmitResult::Finished).then_some(ValueBridgeFailure::OutputFinished);
            writer.retire(failure, result);
        }
        writer
    }

    pub fn active(&self) -> bool {
        self.result == WriteResult::Continue
            && self.lease.as_ref().is_some_and(FormOutputLease::is_open)
    }

    pub fn write(&mut self, event: ValueEvent<'_>) -> WriteResult {
        if self.result != WriteResult::Continue {
            return self.result;
        }
        if !self.lease.as_ref().is_some_and(FormOutputLease::is_open) {
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
        };
        self.handle(result)
    }

    pub fn finish(mut self) -> WriteResult {
        if self.result != WriteResult::Continue {
            return self.result;
        }
        if !self.lease.as_ref().is_some_and(FormOutputLease::is_open) {
            self.retire(None, WriteResult::Inactive);
            return self.result;
        }
        let Some(printer) = self.printer.take() else {
            return self.fail(
                ValueBridgeFailure::InvalidSequence,
                WriteResult::InvalidSequence,
            );
        };
        if self.root_policy == RootPolicy::ExactlyOne && printer.root_values() != 1 {
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
            Err(PrintError::Output(EmitResult::Finished)) => {
                self.fail(ValueBridgeFailure::OutputFinished, WriteResult::Finished)
            }
            Err(PrintError::Output(result)) => {
                self.retire(None, emit_result(result));
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::output::{OutputStream, channel};
    use super::super::{ExecutionIo, ValueBridgeState};
    use super::*;

    #[tokio::test]
    async fn concatenates_any_number_of_println_roots() {
        let (io, mut stream) = execution_io();
        let terminal = io.output_sink();
        let mut writer = NativeValueWriter::println(form_lease(&io));

        assert_eq!(writer.write(ValueEvent::BeginString), WriteResult::Continue);
        assert_eq!(
            writer.write(ValueEvent::StringChunk("created: ")),
            WriteResult::Continue
        );
        assert_eq!(writer.write(ValueEvent::EndString), WriteResult::Continue);
        assert_eq!(writer.write(ValueEvent::Integer(12)), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert_eq!(io.close_form_output(), None);
        terminal.finish();

        assert_eq!(collect(&mut stream).await, "created: 12\n");
    }

    #[test]
    fn eval_value_requires_exactly_one_root() {
        let (io, _stream) = execution_io();
        let writer = NativeValueWriter::eval_value(form_lease(&io));

        assert_eq!(writer.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            io.close_form_output(),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn eval_value_accepts_one_nested_root_and_rejects_two_roots() {
        let (one_io, _one_stream) = execution_io();
        let mut one = NativeValueWriter::eval_value(form_lease(&one_io));
        assert_eq!(one.write(ValueEvent::BeginList), WriteResult::Continue);
        assert_eq!(one.write(ValueEvent::Integer(1)), WriteResult::Continue);
        assert_eq!(one.write(ValueEvent::EndList), WriteResult::Continue);
        assert_eq!(one.finish(), WriteResult::Continue);
        assert_eq!(one_io.close_form_output(), None);

        let (two_io, _two_stream) = execution_io();
        let mut two = NativeValueWriter::eval_value(form_lease(&two_io));
        assert_eq!(two.write(ValueEvent::Integer(1)), WriteResult::Continue);
        assert_eq!(two.write(ValueEvent::Integer(2)), WriteResult::Continue);
        assert_eq!(two.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            two_io.close_form_output(),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn dropping_an_active_writer_records_an_abandoned_bridge() {
        let (io, _stream) = execution_io();
        drop(NativeValueWriter::println(form_lease(&io)));

        assert_eq!(io.close_form_output(), Some(ValueBridgeFailure::Abandoned));
    }

    #[test]
    fn terminal_writer_releases_its_formatter_and_execution_io() {
        let (io, stream) = execution_io();
        let lease = form_lease(&io);
        drop(stream);

        let writer = NativeValueWriter::println(lease);
        assert_eq!(writer.result, WriteResult::Disconnected);
        assert!(writer.printer.is_none());
        assert!(writer.lease.is_none());
        assert_eq!(io.close_form_output(), None);
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

    fn form_lease(io: &Arc<ExecutionIo>) -> FormOutputLease {
        io.begin_form_output();
        io.acquire_form_output().expect("form output is open")
    }

    async fn collect(stream: &mut OutputStream) -> String {
        let mut output = String::new();
        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }
        output
    }
}
