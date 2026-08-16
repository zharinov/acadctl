use super::super::{ValueBridgeFailure, ValueOutputLease};
use super::printer::{PrintError, ValuePrinter};

pub use crate::ffi::NativeValueWriteResult as WriteResult;

pub enum ValueEvent<'a> {
    Invalid,
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

pub struct NativeValueWriter {
    state: WriterState,
}

enum WriterState {
    Inactive,
    Active {
        printer: ValuePrinter,
        lease: ValueOutputLease,
    },
    Retired(WriteResult),
}

impl WriterState {
    fn retired(lease: ValueOutputLease, result: WriteResult) -> Self {
        Self::Retired(Self::release(lease, result))
    }

    fn result(&self) -> WriteResult {
        match self {
            Self::Inactive => WriteResult::Inactive,
            Self::Active { .. } => WriteResult::Continue,
            Self::Retired(result) => *result,
        }
    }

    fn retire(&mut self, result: WriteResult) {
        let previous = std::mem::replace(self, Self::Retired(result));

        if let Self::Active { lease, .. } = previous {
            Self::release(lease, result);
        }
    }

    fn release(lease: ValueOutputLease, result: WriteResult) -> WriteResult {
        lease.release(Self::bridge_failure(result));

        result
    }

    const fn bridge_failure(result: WriteResult) -> Option<ValueBridgeFailure> {
        match result {
            WriteResult::InvalidSequence => Some(ValueBridgeFailure::InvalidSequence),
            WriteResult::LimitExceeded => Some(ValueBridgeFailure::LimitExceeded),
            WriteResult::Finished => Some(ValueBridgeFailure::OutputFinished),
            WriteResult::Cancelled => Some(ValueBridgeFailure::PostCommitCancelled),
            _ => None,
        }
    }
}

impl NativeValueWriter {
    pub fn inactive() -> Self {
        Self {
            state: WriterState::Inactive,
        }
    }

    pub(crate) fn eval_value(lease: ValueOutputLease) -> Self {
        Self::new(lease)
    }

    fn new(lease: ValueOutputLease) -> Self {
        let sink = lease.output_sink();
        let result = sink.emit("");

        if result != WriteResult::Continue {
            return Self {
                state: WriterState::retired(lease, result),
            };
        }

        Self {
            state: WriterState::Active {
                printer: ValuePrinter::new(sink),
                lease,
            },
        }
    }

    pub fn active(&self) -> bool {
        matches!(&self.state, WriterState::Active { lease, .. } if lease.is_open())
    }

    pub fn write(&mut self, event: ValueEvent<'_>) -> WriteResult {
        let result = self.state.result();
        if result != WriteResult::Continue {
            return result;
        }

        if !self.active() {
            self.retire(WriteResult::Inactive);

            return self.state.result();
        }

        let WriterState::Active { printer, .. } = &mut self.state else {
            unreachable!("continuing value writer has active state");
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

        self.handle(result)
    }

    pub fn finish(self) -> WriteResult {
        let (printer, lease) = match self.state {
            WriterState::Inactive => return WriteResult::Inactive,
            WriterState::Retired(result) => return result,
            WriterState::Active { printer, lease } => (printer, lease),
        };

        if !lease.is_open() {
            return WriterState::release(lease, WriteResult::Inactive);
        }

        if printer.root_values() != 1 {
            return WriterState::release(lease, WriteResult::InvalidSequence);
        }

        let result = match printer.finish() {
            Ok(()) => WriteResult::Continue,
            Err(PrintError::InvalidSequence) => WriteResult::InvalidSequence,
            Err(PrintError::LimitExceeded) => WriteResult::LimitExceeded,
            Err(PrintError::Output(result)) => result,
        };

        WriterState::release(lease, result)
    }

    fn handle(&mut self, result: Result<(), PrintError>) -> WriteResult {
        match result {
            Ok(()) => WriteResult::Continue,
            Err(PrintError::InvalidSequence) => self.fail(WriteResult::InvalidSequence),
            Err(PrintError::LimitExceeded) => self.fail(WriteResult::LimitExceeded),
            Err(PrintError::Output(result)) => {
                self.retire(result);

                self.state.result()
            }
        }
    }

    fn fail(&mut self, result: WriteResult) -> WriteResult {
        self.retire(result);

        self.state.result()
    }

    fn retire(&mut self, result: WriteResult) {
        self.state.retire(result);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::super::output::{OutputStream, channel};
    use super::super::super::{ExecIo, ValueBridgeState};
    use super::*;

    #[test]
    fn eval_value_requires_exactly_one_root() {
        let (io, _stream) = execution_io();
        let writer = NativeValueWriter::eval_value(eval_value_lease(&io));

        assert_eq!(writer.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            io.close_value_output(),
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
        assert_eq!(one_io.close_value_output(), None);

        let (two_io, _two_stream) = execution_io();
        let mut two = NativeValueWriter::eval_value(eval_value_lease(&two_io));
        assert_eq!(two.write(ValueEvent::Integer(1)), WriteResult::Continue);
        assert_eq!(two.write(ValueEvent::Integer(2)), WriteResult::Continue);
        assert_eq!(two.finish(), WriteResult::InvalidSequence);
        assert_eq!(
            two_io.close_value_output(),
            Some(ValueBridgeFailure::InvalidSequence)
        );
    }

    #[test]
    fn eval_value_epoch_requires_exactly_one_writer_claim() {
        let (io, _stream) = execution_io();
        io.begin_value_output();
        assert_eq!(
            io.close_value_output(),
            Some(ValueBridgeFailure::MissingValue)
        );

        io.begin_value_output();
        let mut writer =
            NativeValueWriter::eval_value(io.acquire_value_output().expect("first claim succeeds"));
        assert!(io.acquire_value_output().is_none());
        assert_eq!(writer.write(ValueEvent::Nil), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert!(io.acquire_value_output().is_none());
        assert_eq!(io.close_value_output(), None);
    }

    #[test]
    fn disconnected_eval_writer_still_satisfies_the_required_claim() {
        let (io, stream) = execution_io();
        let lease = eval_value_lease(&io);
        drop(stream);

        let writer = NativeValueWriter::eval_value(lease);
        assert_eq!(writer.state.result(), WriteResult::Disconnected);
        assert_eq!(io.close_value_output(), None);
    }

    #[test]
    fn dropping_an_active_writer_records_an_abandoned_bridge() {
        let (io, _stream) = execution_io();
        drop(NativeValueWriter::eval_value(eval_value_lease(&io)));

        assert_eq!(io.close_value_output(), Some(ValueBridgeFailure::Abandoned));
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

    fn eval_value_lease(io: &Arc<ExecIo>) -> ValueOutputLease {
        io.begin_value_output();
        io.acquire_value_output()
            .expect("eval value output is open")
    }
}
