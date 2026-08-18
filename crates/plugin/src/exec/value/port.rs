use super::super::{ValueBridgeFailure, ValueOutputLease, println};
use super::event::{OutputEvent, ProtocolViolation};
use super::printer::{PrintError, ValuePrinter};

pub use crate::ffi::NativeOutputWriteResult as WriteResult;

pub enum ValueEvent<'a> {
    BeginList,
    EndList,
    Dot,
    Nil,
    True,
    Integer(i64),
    Real(f64),
    BeginString,
    StringChunk(&'a str),
    EndString,
    BeginSymbol,
    SymbolChunk(&'a str),
    EndSymbol,
    Entity(Option<&'a str>),
    SelectionSet,
    VlaObject,
    File,
    Function,
    ErrorObject,
    Object(Option<&'a str>),
    Cycle,
    TooDeep,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputPortMode {
    Form,
    EvalValue,
}

pub struct NativeOutputPort {
    lease: Option<ValueOutputLease>,
    printer: Option<ValuePrinter>,
    mode: OutputPortMode,
    completed_values: usize,
    result: WriteResult,
}

impl NativeOutputPort {
    pub fn inactive() -> Self {
        Self {
            lease: None,
            printer: None,
            mode: OutputPortMode::Form,
            completed_values: 0,
            result: WriteResult::Inactive,
        }
    }

    pub(crate) fn form(lease: ValueOutputLease) -> Self {
        Self::new(lease, OutputPortMode::Form)
    }

    pub(crate) fn eval_value(lease: ValueOutputLease) -> Self {
        Self::new(lease, OutputPortMode::EvalValue)
    }

    fn new(lease: ValueOutputLease, mode: OutputPortMode) -> Self {
        let result = lease.output_sink().emit("");
        Self {
            lease: Some(lease),
            printer: None,
            mode,
            completed_values: 0,
            result,
        }
    }

    pub fn claimed(&self) -> bool {
        self.lease.as_ref().is_some_and(ValueOutputLease::is_open)
    }

    pub(crate) fn write(
        &mut self,
        event: Result<OutputEvent<'_>, ProtocolViolation>,
    ) -> WriteResult {
        let Ok(event) = event else {
            return self.fail(WriteResult::InvalidSequence);
        };

        if self.result != WriteResult::Continue {
            return self.result;
        }

        if !self.claimed() {
            self.result = WriteResult::Inactive;
            return self.result;
        }

        match event {
            OutputEvent::BeginValue => self.begin_value(),
            OutputEvent::EndValue => self.end_value(),
            OutputEvent::Println(text) => self.println(text),
            OutputEvent::InvalidPrintln => self.fail(WriteResult::InvalidSequence),
            OutputEvent::Value(event) => self.value(event),
        }
    }

    pub fn invalidate(&mut self) {
        self.fail(WriteResult::InvalidSequence);
    }

    pub fn finish(mut self) -> WriteResult {
        let failure = self.completion_failure();

        if let Some(lease) = self.lease.take() {
            lease.release(failure);
        }

        self.result
    }

    fn completion_failure(&mut self) -> Option<ValueBridgeFailure> {
        if self.result != WriteResult::Continue {
            return bridge_failure(self.mode, self.result);
        }

        if self.printer.is_some() {
            self.result = WriteResult::InvalidSequence;
            return Some(ValueBridgeFailure::Abandoned);
        }

        if self.mode == OutputPortMode::EvalValue && self.completed_values != 1 {
            self.result = WriteResult::InvalidSequence;
            return Some(ValueBridgeFailure::MissingValue);
        }

        None
    }

    fn begin_value(&mut self) -> WriteResult {
        if self.printer.is_some()
            || (self.mode == OutputPortMode::EvalValue && self.completed_values != 0)
        {
            return self.fail(WriteResult::InvalidSequence);
        }

        let Some(lease) = &self.lease else {
            return self.fail(WriteResult::Inactive);
        };
        self.printer = Some(ValuePrinter::new(lease.output_sink()));
        WriteResult::Continue
    }

    fn end_value(&mut self) -> WriteResult {
        let Some(printer) = self.printer.take() else {
            return self.fail(WriteResult::InvalidSequence);
        };

        if printer.root_values() != 1 {
            return self.fail(WriteResult::InvalidSequence);
        }

        match printer.finish() {
            Ok(()) => {
                self.completed_values += 1;
                WriteResult::Continue
            }
            Err(error) => self.print_error(error),
        }
    }

    fn println(&mut self, text: &str) -> WriteResult {
        if self.mode != OutputPortMode::Form || self.printer.is_some() {
            return self.fail(WriteResult::InvalidSequence);
        }

        let Some(lease) = &self.lease else {
            return self.fail(WriteResult::Inactive);
        };
        let result = println::write(&lease.output_sink(), text);
        if result == WriteResult::Continue {
            result
        } else {
            self.fail(result)
        }
    }

    fn value(&mut self, event: ValueEvent<'_>) -> WriteResult {
        let Some(printer) = &mut self.printer else {
            return self.fail(WriteResult::InvalidSequence);
        };

        let result = match event {
            ValueEvent::BeginList => printer.begin_list(),
            ValueEvent::EndList => printer.end_list(),
            ValueEvent::Dot => printer.dot(),
            ValueEvent::Nil => printer.nil(),
            ValueEvent::True => printer.true_value(),
            ValueEvent::Integer(value) => printer.integer(value),
            ValueEvent::Real(value) => printer.real(value),
            ValueEvent::BeginString => printer.begin_string(),
            ValueEvent::StringChunk(text) => printer.string_chunk(text),
            ValueEvent::EndString => printer.end_string(),
            ValueEvent::BeginSymbol => printer.begin_symbol(),
            ValueEvent::SymbolChunk(text) => printer.symbol_chunk(text),
            ValueEvent::EndSymbol => printer.end_symbol(),
            ValueEvent::Entity(handle) => printer.entity(handle),
            ValueEvent::SelectionSet => printer.selection_set(),
            ValueEvent::VlaObject => printer.vla_object(),
            ValueEvent::File => printer.file(),
            ValueEvent::Function => printer.function(),
            ValueEvent::ErrorObject => printer.error_object(),
            ValueEvent::Object(label) => printer.object(label),
            ValueEvent::Cycle => printer.cycle(),
            ValueEvent::TooDeep => printer.too_deep(),
        };

        match result {
            Ok(()) => WriteResult::Continue,
            Err(error) => self.print_error(error),
        }
    }

    fn print_error(&mut self, error: PrintError) -> WriteResult {
        match error {
            PrintError::InvalidSequence => self.fail(WriteResult::InvalidSequence),
            PrintError::LimitExceeded => self.fail(WriteResult::LimitExceeded),
            PrintError::Output(result) => self.fail(result),
        }
    }

    fn fail(&mut self, result: WriteResult) -> WriteResult {
        self.printer = None;
        self.result = result;
        result
    }
}

impl Drop for NativeOutputPort {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.release(Some(ValueBridgeFailure::Abandoned));
        }
    }
}

fn bridge_failure(mode: OutputPortMode, result: WriteResult) -> Option<ValueBridgeFailure> {
    match result {
        WriteResult::InvalidSequence => Some(ValueBridgeFailure::InvalidSequence),
        WriteResult::LimitExceeded => Some(ValueBridgeFailure::LimitExceeded),
        WriteResult::Finished => Some(ValueBridgeFailure::OutputFinished),
        WriteResult::Cancelled if mode == OutputPortMode::EvalValue => {
            Some(ValueBridgeFailure::PostCommitCancelled)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::super::output::{OutputStream, channel};
    use super::super::super::{ExecIo, ValueBridgeState, ValueOutputKind};
    use super::*;

    #[tokio::test]
    async fn form_port_orders_lines_and_repeated_values() {
        let (io, stream) = execution_io();
        let terminal = io.output_sink();
        let mut port = NativeOutputPort::form(lease(&io, ValueOutputKind::Form));

        assert_eq!(
            port.write(Ok(OutputEvent::Println("layers"))),
            WriteResult::Continue
        );
        for value in [1, 2] {
            assert_eq!(
                port.write(Ok(OutputEvent::BeginValue)),
                WriteResult::Continue
            );
            assert_eq!(
                port.write(Ok(OutputEvent::Value(ValueEvent::Integer(value)))),
                WriteResult::Continue
            );
            assert_eq!(port.write(Ok(OutputEvent::EndValue)), WriteResult::Continue);
        }

        assert_eq!(port.finish(), WriteResult::Continue);
        assert_eq!(io.close_value_output(ValueOutputKind::Form), None);
        terminal.finish();
        assert_eq!(collect(stream).await, "layers\n1\n2\n");
    }

    #[test]
    fn eval_port_requires_exactly_one_value_and_rejects_println() {
        let (missing, _stream) = execution_io();
        let port = NativeOutputPort::eval_value(lease(&missing, ValueOutputKind::EvalValue));
        assert_eq!(port.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            missing.close_value_output(ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::MissingValue)
        );

        let (explicit, _stream) = execution_io();
        let mut port = NativeOutputPort::eval_value(lease(&explicit, ValueOutputKind::EvalValue));
        assert_eq!(
            port.write(Ok(OutputEvent::Println("invalid"))),
            WriteResult::InvalidSequence
        );
        assert_eq!(port.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            explicit.close_value_output(ValueOutputKind::EvalValue),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn disconnected_form_port_stays_claimed_and_suppresses_output() {
        let (io, stream) = execution_io();
        let lease = lease(&io, ValueOutputKind::Form);
        drop(stream);

        let mut port = NativeOutputPort::form(lease);
        assert!(port.claimed());
        assert_eq!(
            port.write(Ok(OutputEvent::BeginValue)),
            WriteResult::Disconnected
        );
        assert_eq!(port.finish(), WriteResult::Disconnected);
        assert_eq!(io.close_value_output(ValueOutputKind::Form), None);
    }

    fn execution_io() -> (Arc<ExecIo>, OutputStream) {
        let (output, stream) = channel();
        (
            Arc::new(ExecIo {
                output,
                bridge: Mutex::new(ValueBridgeState::default()),
            }),
            stream,
        )
    }

    fn lease(io: &Arc<ExecIo>, kind: ValueOutputKind) -> ValueOutputLease {
        io.begin_value_output(kind);
        io.acquire_value_output(kind)
            .expect("output port claim succeeds")
    }

    async fn collect(stream: OutputStream) -> String {
        let mut output = String::new();
        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }
        output
    }
}
