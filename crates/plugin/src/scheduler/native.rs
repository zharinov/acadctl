use acadctl_rpc::{DrawingId, DrawingPath, SavePath};

use crate::drawing::{DrawingRegistry, NativeDocumentKey};
use crate::exec::{ExecOutcome, ExecStepResult};
use crate::ffi::{
    NativeActionKind, NativeActionResult, NativeActionResultKind, NativeCaptureResult,
    NativeCaptureResultKind, NativeExecFinalizationObservation, NativePixelFormat, NativeRowOrder,
};
use crate::screenshot::{CapturedFrame, PixelBounds, PixelFormat, RowOrder};

use super::error::{Error, NativeFailure};
use super::operation::{DocumentContextPolicy, Operation, OperationOutcome};

pub(super) enum NativeCommand {
    Open(DrawingPath),
    Drawing {
        target: NativeDocumentKey,
        operation: NativeDrawingOperation,
    },
}

pub(super) enum NativeDrawingOperation {
    Switch,
    Save {
        path: Option<SavePath>,
    },
    Close {
        discard: bool,
    },
    Capture {
        region: CaptureRegion,
        max_long_edge: u32,
    },
    Undo {
        context: DocumentContextPolicy,
    },
    Redo {
        context: DocumentContextPolicy,
    },
    QueueExecDriver {
        context: DocumentContextPolicy,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CaptureRegion {
    pub(crate) min_x: f64,
    pub(crate) min_y: f64,
    pub(crate) max_x: f64,
    pub(crate) max_y: f64,
}

pub(crate) struct NativeAction {
    state: NativeActionState,
}

enum NativeActionState {
    Idle,
    Issued { job_id: u64, command: NativeCommand },
}

impl NativeCommand {
    pub(super) fn open(path: DrawingPath) -> Self {
        Self::Open(path)
    }

    pub(super) fn save(target: NativeDocumentKey, path: Option<SavePath>) -> Self {
        Self::drawing(target, NativeDrawingOperation::Save { path })
    }

    pub(super) fn switch(target: NativeDocumentKey) -> Self {
        Self::drawing(target, NativeDrawingOperation::Switch)
    }

    pub(super) fn close(target: NativeDocumentKey, discard: bool) -> Self {
        Self::drawing(target, NativeDrawingOperation::Close { discard })
    }

    pub(super) fn capture(
        target: NativeDocumentKey,
        region: CaptureRegion,
        max_long_edge: u32,
    ) -> Self {
        Self::drawing(
            target,
            NativeDrawingOperation::Capture {
                region,
                max_long_edge,
            },
        )
    }

    pub(super) fn undo(target: NativeDocumentKey, context: DocumentContextPolicy) -> Self {
        Self::drawing(target, NativeDrawingOperation::Undo { context })
    }

    pub(super) fn redo(target: NativeDocumentKey, context: DocumentContextPolicy) -> Self {
        Self::drawing(target, NativeDrawingOperation::Redo { context })
    }

    pub(super) fn queue_exec_driver(
        target: NativeDocumentKey,
        context: DocumentContextPolicy,
    ) -> Self {
        Self::drawing(target, NativeDrawingOperation::QueueExecDriver { context })
    }

    fn drawing(target: NativeDocumentKey, operation: NativeDrawingOperation) -> Self {
        Self::Drawing { target, operation }
    }

    pub(super) fn target(&self) -> Option<NativeDocumentKey> {
        match self {
            Self::Open(_) => None,
            Self::Drawing { target, .. } => Some(*target),
        }
    }

    fn kind(&self) -> NativeActionKind {
        match self {
            Self::Open(_) => NativeActionKind::Open,
            Self::Drawing { operation, .. } => operation.kind(),
        }
    }
}

impl NativeDrawingOperation {
    fn kind(&self) -> NativeActionKind {
        match self {
            Self::Switch => NativeActionKind::Switch,
            Self::Save { .. } => NativeActionKind::Save,
            Self::Close { .. } => NativeActionKind::Close,
            Self::Capture { .. } => NativeActionKind::Capture,
            Self::Undo { .. } => NativeActionKind::Undo,
            Self::Redo { .. } => NativeActionKind::Redo,
            Self::QueueExecDriver { .. } => NativeActionKind::QueueExecDriver,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewportCapture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stride: usize,
    pub(crate) pixel_format: PixelFormat,
    pub(crate) row_order: RowOrder,
    pub(crate) realistic_style: bool,
    pub(crate) bounds: PixelBounds,
    pub(crate) pixels: Vec<u8>,
}

impl ViewportCapture {
    const MAX_DIMENSION: u32 = 16_384;
    const MAX_BYTES: usize = 128 * 1024 * 1024;

    pub(crate) fn frame(&self) -> CapturedFrame<'_> {
        CapturedFrame {
            width: self.width,
            height: self.height,
            stride: self.stride,
            pixel_format: self.pixel_format,
            row_order: self.row_order,
            pixels: &self.pixels,
        }
    }

    fn from_native(result: &NativeCaptureResult, pixels: &[u8]) -> Result<Self, Error> {
        let pixel_format = match result.pixel_format {
            NativePixelFormat::Bgra8 => PixelFormat::Bgra8,
            NativePixelFormat::Bgrx8 => PixelFormat::Bgrx8,
            _ => return Err(Error::CaptureInvalid("unknown native pixel format".into())),
        };
        let row_order = match result.row_order {
            NativeRowOrder::TopDown => RowOrder::TopDown,
            NativeRowOrder::BottomUp => RowOrder::BottomUp,
            _ => return Err(Error::CaptureInvalid("unknown native row order".into())),
        };
        let bounds = PixelBounds::new(
            result.crop_left,
            result.crop_top,
            result.crop_width,
            result.crop_height,
        )
        .map_err(|error| Error::CaptureInvalid(error.to_string()))?;

        if result.width == 0
            || result.height == 0
            || result.width > Self::MAX_DIMENSION
            || result.height > Self::MAX_DIMENSION
        {
            return Err(Error::CaptureInvalid(
                "native capture dimensions are empty or exceed the supported limit".into(),
            ));
        }

        let row_bytes = usize::try_from(result.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| Error::CaptureInvalid("native capture dimensions overflow".into()))?;
        let required_bytes = usize::try_from(result.height)
            .ok()
            .and_then(|height| result.stride.checked_mul(height))
            .ok_or_else(|| Error::CaptureInvalid("native capture dimensions overflow".into()))?;

        if result.stride < row_bytes
            || required_bytes > Self::MAX_BYTES
            || pixels.len() != required_bytes
        {
            return Err(Error::CaptureInvalid(
                "native capture stride or buffer length is invalid".into(),
            ));
        }

        Ok(Self {
            width: result.width,
            height: result.height,
            stride: result.stride,
            pixel_format,
            row_order,
            realistic_style: result.realistic_style,
            bounds,
            pixels: pixels.to_vec(),
        })
    }
}

pub(super) fn interpret_capture(
    result: &NativeCaptureResult,
    pixels: &[u8],
    drawing_id: DrawingId,
) -> Result<ViewportCapture, Error> {
    match result.kind {
        NativeCaptureResultKind::Success => ViewportCapture::from_native(result, pixels),
        NativeCaptureResultKind::DrawingGone => Err(Error::DrawingGone),
        NativeCaptureResultKind::DrawingGenerationChanged => Err(Error::DrawingGenerationChanged),
        NativeCaptureResultKind::NotActive => Err(Error::NotActive(drawing_id)),
        NativeCaptureResultKind::NotQuiescent => Err(Error::NotQuiescent),
        NativeCaptureResultKind::Unavailable => {
            Err(Error::CaptureUnavailable(result.detail.clone()))
        }
        NativeCaptureResultKind::Invalid => Err(Error::CaptureInvalid(result.detail.clone())),
        NativeCaptureResultKind::RestoreFailed => {
            Err(Error::CaptureRestoreFailed(result.detail.clone()))
        }
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

impl NativeAction {
    pub(super) fn idle() -> Self {
        Self {
            state: NativeActionState::Idle,
        }
    }

    pub(super) fn issue(job_id: u64, command: NativeCommand) -> Self {
        Self {
            state: NativeActionState::Issued { job_id, command },
        }
    }

    pub(crate) fn job_id(&self) -> u64 {
        let NativeActionState::Issued { job_id, .. } = &self.state else {
            panic!("idle native action has no job ID");
        };

        *job_id
    }

    pub(crate) fn kind(&self) -> NativeActionKind {
        match &self.state {
            NativeActionState::Idle => NativeActionKind::None,
            NativeActionState::Issued { command, .. } => command.kind(),
        }
    }

    pub(crate) fn document_token(&self) -> usize {
        self.drawing_target().document_token
    }

    pub(crate) fn database_token(&self) -> usize {
        self.drawing_target().database_token
    }

    pub(crate) fn open_path(&self) -> &str {
        match &self.state {
            NativeActionState::Issued {
                command: NativeCommand::Open(path),
                ..
            } => path.as_str(),
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not open")
            }
        }
    }

    pub(crate) fn close_discard(&self) -> bool {
        match &self.state {
            NativeActionState::Issued {
                command:
                    NativeCommand::Drawing {
                        operation: NativeDrawingOperation::Close { discard },
                        ..
                    },
                ..
            } => *discard,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not close")
            }
        }
    }

    pub(crate) fn save_path(&self) -> &str {
        match &self.state {
            NativeActionState::Issued {
                command:
                    NativeCommand::Drawing {
                        operation: NativeDrawingOperation::Save { path },
                        ..
                    },
                ..
            } => path.as_ref().map_or("", SavePath::as_str),
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not save")
            }
        }
    }

    pub(crate) fn force_document_context(&self) -> bool {
        match &self.state {
            NativeActionState::Issued {
                command:
                    NativeCommand::Drawing {
                        operation:
                            NativeDrawingOperation::Undo { context }
                            | NativeDrawingOperation::Redo { context }
                            | NativeDrawingOperation::QueueExecDriver { context },
                        ..
                    },
                ..
            } => *context == DocumentContextPolicy::ForceTemporary,
            NativeActionState::Idle | NativeActionState::Issued { .. } => false,
        }
    }

    pub(crate) fn capture_min_x(&self) -> f64 {
        self.capture_region().min_x
    }

    pub(crate) fn capture_min_y(&self) -> f64 {
        self.capture_region().min_y
    }

    pub(crate) fn capture_max_x(&self) -> f64 {
        self.capture_region().max_x
    }

    pub(crate) fn capture_max_y(&self) -> f64 {
        self.capture_region().max_y
    }

    pub(crate) fn capture_max_long_edge(&self) -> u32 {
        let NativeActionState::Issued {
            command:
                NativeCommand::Drawing {
                    operation: NativeDrawingOperation::Capture { max_long_edge, .. },
                    ..
                },
            ..
        } = &self.state
        else {
            panic!("native action is not capture");
        };

        *max_long_edge
    }

    fn capture_region(&self) -> CaptureRegion {
        let NativeActionState::Issued {
            command:
                NativeCommand::Drawing {
                    operation: NativeDrawingOperation::Capture { region, .. },
                    ..
                },
            ..
        } = &self.state
        else {
            panic!("native action is not capture");
        };

        *region
    }

    fn drawing_target(&self) -> NativeDocumentKey {
        match &self.state {
            NativeActionState::Issued {
                command: NativeCommand::Drawing { target, .. },
                ..
            } => *target,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action has no drawing target")
            }
        }
    }
}

impl NativeExecFinalizationObservation {
    fn native_state_unproved(&self) -> bool {
        self.undo_group_may_be_open
            || self.bridge_symbols_may_be_retained
            || self.staged_form_may_be_retained
            || self.output_port_active
            || self.terminal_cleanup_failed
    }

    fn only_symbol_cleanup_unproved(&self) -> bool {
        (self.bridge_symbols_may_be_retained || self.terminal_cleanup_failed)
            && !self.undo_group_may_be_open
            && !self.staged_form_may_be_retained
            && !self.output_port_active
    }
}

impl Operation {
    pub(super) fn complete_native(
        &mut self,
        mut result: NativeActionResult,
        drawings: &DrawingRegistry,
        native_target: Option<NativeDocumentKey>,
    ) -> Result<OperationOutcome, Error> {
        if matches!(
            self,
            Operation::Execute { execution, .. } if execution.outcome().is_some()
        ) && !matches!(
            result.kind,
            NativeActionResultKind::DocumentContextRestoreFailed
                | NativeActionResultKind::ExecBridgeFinalizationFailed
                | NativeActionResultKind::ExecBridgeSymbolsClearFailed
                | NativeActionResultKind::ExecBridgeFailed
        ) {
            return self.complete(drawings, native_target);
        }

        if result.kind == NativeActionResultKind::ExecBridgeSymbolsClearFailed
            && let Operation::Execute { execution, .. } = self
            && matches!(execution.outcome(), Some(ExecOutcome::Failure(_)))
        {
            return self.complete(drawings, native_target);
        }

        if matches!(
            result.kind,
            NativeActionResultKind::DocumentContextRestoreFailed
                | NativeActionResultKind::ExecBridgeFinalizationFailed
        ) && let Operation::Execute { execution, .. } = self
            && execution.outcome().is_some()
        {
            let recorded = execution.record_bridge_finalization_failure(ExecStepResult {
                kind: crate::exec::ExecStepResultKind::NativeError,
                native_status: result.native_status,
                lisp_errno: 0,
                detail: std::mem::take(&mut result.native_detail),
                bridge_symbols_clear_status: 0,
            });
            debug_assert!(recorded);

            return self.complete(drawings, native_target);
        }

        interpret(result, self)?;
        self.complete(drawings, native_target)
    }
}

pub(super) fn interpret(result: NativeActionResult, operation: &Operation) -> Result<(), Error> {
    let failure = NativeFailure {
        status: result.native_status,
        detail: result.native_detail,
    };

    match result.kind {
        NativeActionResultKind::Success => Ok(()),
        NativeActionResultKind::DrawingGone => Err(Error::DrawingGone),
        NativeActionResultKind::DrawingGenerationChanged => Err(Error::DrawingGenerationChanged),
        NativeActionResultKind::Unnamed => Err(operation
            .drawing_id()
            .map(Error::Unnamed)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::ReadOnly => Err(operation
            .drawing_id()
            .map(Error::ReadOnly)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::NotActive => Err(operation
            .drawing_id()
            .map(Error::NotActive)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::Dirty => Err(operation
            .drawing_id()
            .map(Error::Dirty)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::DestinationExists => Err(operation
            .drawing_id()
            .map(Error::DestinationExists)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::OpenFailed => Err(Error::OpenFailed(failure)),
        NativeActionResultKind::SwitchFailed => Err(Error::SwitchFailed(failure)),
        NativeActionResultKind::SaveFailed => Err(Error::SaveFailed(failure)),
        NativeActionResultKind::CloseFailed => Err(Error::CloseFailed(failure)),
        NativeActionResultKind::HistoryFailed => {
            let Operation::History { direction, .. } = operation else {
                return Err(Error::UnknownResult(result.kind.repr));
            };

            Err(Error::HistoryFailed {
                direction: *direction,
                failure,
            })
        }
        NativeActionResultKind::NotQuiescent => Err(Error::NotQuiescent),
        NativeActionResultKind::UndoDisabled => Err(Error::UndoDisabled),
        NativeActionResultKind::DocumentContextFailed => Err(Error::DocumentContextFailed(failure)),
        NativeActionResultKind::DocumentContextRestoreFailed => {
            Err(Error::DocumentContextRestoreFailed(failure))
        }
        NativeActionResultKind::ExecBridgeFinalizationFailed => {
            Err(Error::ExecBridgeFinalizationFailed(failure))
        }
        NativeActionResultKind::ExecBridgeSymbolsClearFailed => {
            Err(Error::ExecBridgeSymbolsClearFailed(failure))
        }
        NativeActionResultKind::ExecBridgeFailed => Err(Error::ExecBridgeFailed(failure)),
        NativeActionResultKind::CaptureUnavailable => {
            Err(Error::CaptureUnavailable(failure.detail))
        }
        NativeActionResultKind::CaptureInvalid => Err(Error::CaptureInvalid(failure.detail)),
        NativeActionResultKind::CaptureRestoreFailed => {
            Err(Error::CaptureRestoreFailed(failure.detail))
        }
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

pub(super) fn native_result_requires_quarantine(kind: NativeActionResultKind) -> bool {
    matches!(
        kind,
        NativeActionResultKind::DocumentContextRestoreFailed
            | NativeActionResultKind::CaptureRestoreFailed
            | NativeActionResultKind::ExecBridgeFinalizationFailed
            | NativeActionResultKind::ExecBridgeSymbolsClearFailed
    )
}

pub(super) struct ExecFinalizationDecision {
    pub(super) result: NativeActionResult,
    pub(super) quarantine: bool,
}

pub(super) fn classify_execution_finalization(
    mut result: NativeActionResult,
    observation: NativeExecFinalizationObservation,
) -> ExecFinalizationDecision {
    let native_state_unproved = observation.native_state_unproved();
    let quarantine = native_state_unproved || native_result_requires_quarantine(result.kind);
    let preserve_symbol_failure = result.kind
        == NativeActionResultKind::ExecBridgeSymbolsClearFailed
        && observation.only_symbol_cleanup_unproved();

    if native_state_unproved
        && result.kind != NativeActionResultKind::DocumentContextRestoreFailed
        && !preserve_symbol_failure
    {
        result.kind = NativeActionResultKind::ExecBridgeFinalizationFailed;
    }

    ExecFinalizationDecision { result, quarantine }
}

pub(super) fn schedule_native_actions() {
    let status = wake_native_actions();

    if status != 0 {
        super::queue::wake_failed();
    }
}

#[cfg(not(test))]
fn wake_native_actions() -> i32 {
    unsafe extern "C" {
        fn acadctl_wake_native_actions() -> i32;
    }

    // SAFETY: the native bridge exports this no-argument function for the plugin's lifetime.
    unsafe { acadctl_wake_native_actions() }
}

#[cfg(test)]
fn wake_native_actions() -> i32 {
    0
}
