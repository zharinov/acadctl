use std::sync::{Arc, Mutex};

use super::output::OutputSink;

pub(crate) struct ExecIo {
    pub(super) output: OutputSink,
    pub(super) bridge: Mutex<ValueBridgeState>,
}

#[derive(Default)]
pub(crate) struct ValueBridgeState {
    generation: u64,
    open_kind: Option<ValueOutputKind>,
    port_active: bool,
    port_claimed: bool,
    failure: Option<ValueBridgeFailure>,
}

pub(crate) struct ValueOutputLease {
    io: Arc<ExecIo>,
    generation: u64,
    kind: ValueOutputKind,
    released: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueOutputKind {
    Form,
    EvalValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueBridgeFailure {
    InvalidSequence,
    LimitExceeded,
    OutputFinished,
    PostCommitCancelled,
    Abandoned,
    MissingValue,
}

impl ExecIo {
    pub(crate) fn output_sink(&self) -> OutputSink {
        self.output.clone()
    }

    pub(super) fn begin_value_output(&self, kind: ValueOutputKind) {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.open_kind.is_some() || state.port_active {
            state.failure.get_or_insert(ValueBridgeFailure::Abandoned);
        }

        state.generation = state.generation.wrapping_add(1).max(1);
        state.open_kind = Some(kind);
        state.port_active = false;
        state.port_claimed = false;
    }

    pub(super) fn acquire_value_output(
        self: &Arc<Self>,
        kind: ValueOutputKind,
    ) -> Option<ValueOutputLease> {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.open_kind != Some(kind)
            || state.failure.is_some()
            || state.port_active
            || state.port_claimed
        {
            return None;
        }

        state.port_active = true;
        state.port_claimed = true;
        Some(ValueOutputLease {
            io: Arc::clone(self),
            generation: state.generation,
            kind,
            released: false,
        })
    }

    pub(super) fn close_value_output(&self, kind: ValueOutputKind) -> Option<ValueBridgeFailure> {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.open_kind != Some(kind) {
            state
                .failure
                .get_or_insert(ValueBridgeFailure::InvalidSequence);
        }

        state.open_kind = None;

        if state.port_active {
            state.failure.get_or_insert(ValueBridgeFailure::Abandoned);
        }

        state.port_active = false;

        if kind == ValueOutputKind::EvalValue && !state.port_claimed {
            state
                .failure
                .get_or_insert(ValueBridgeFailure::MissingValue);
        }

        state.failure.take()
    }

    fn value_output_is_open(&self, generation: u64, kind: ValueOutputKind) -> bool {
        let state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.open_kind == Some(kind)
            && state.generation == generation
            && state.port_active
            && state.failure.is_none()
    }

    fn release_value_output(
        &self,
        generation: u64,
        kind: ValueOutputKind,
        failure: Option<ValueBridgeFailure>,
    ) {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.generation != generation || state.open_kind != Some(kind) {
            return;
        }

        if let Some(failure) = failure {
            state.failure.get_or_insert(failure);
        }

        state.port_active = false;
    }
}

impl ValueOutputLease {
    pub(crate) fn output_sink(&self) -> OutputSink {
        self.io.output_sink()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.io.value_output_is_open(self.generation, self.kind)
    }

    pub(crate) fn release(mut self, failure: Option<ValueBridgeFailure>) {
        self.io
            .release_value_output(self.generation, self.kind, failure);
        self.released = true;
    }
}

impl Drop for ValueOutputLease {
    fn drop(&mut self) {
        if !self.released {
            self.io.release_value_output(
                self.generation,
                self.kind,
                Some(ValueBridgeFailure::Abandoned),
            );
        }
    }
}

impl ValueBridgeFailure {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::InvalidSequence => "the AutoLISP output bridge emitted an invalid value sequence",
            Self::LimitExceeded => "the AutoLISP output bridge exceeded its structural limit",
            Self::OutputFinished => "execution output ended while AutoLISP was still producing it",
            Self::PostCommitCancelled => "eval result output was cancelled after commit",
            Self::Abandoned => "the AutoLISP output bridge abandoned an unfinished value",
            Self::MissingValue => "the AutoLISP evaluator did not emit its result value",
        }
    }
}
