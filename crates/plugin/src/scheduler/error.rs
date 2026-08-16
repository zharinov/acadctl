use std::fmt;

use acadctl_rpc::{DrawingErrorKind, DrawingId};

use crate::exec::DrawingOutcome;

use super::operation::HistoryDirection;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    SchedulerStateUnavailable,
    ScheduleFailed(i32),
    Stopped,
    PluginStopping,
    DrawingNotFound(DrawingId),
    DrawingGone,
    DrawingGenerationChanged,
    Unnamed(DrawingId),
    ReadOnly(DrawingId),
    Dirty(DrawingId),
    NotDwg,
    OpenFailed(NativeFailure),
    LockFailed(NativeFailure),
    SaveFailed(NativeFailure),
    CloseFailed(NativeFailure),
    HistoryFailed {
        direction: HistoryDirection,
        failure: NativeFailure,
    },
    OpenNotPublished,
    SaveNotPublished,
    CloseNotPublished,
    NotQuiescent,
    UndoDisabled,
    DocumentContextFailed(NativeFailure),
    DocumentContextRestoreFailed(NativeFailure),
    ExecBridgeFinalizationFailed(NativeFailure),
    ExecBridgeSymbolsClearFailed(NativeFailure),
    ExecBridgeFailed(NativeFailure),
    ExecNotFinished,
    MutationCapacity,
    ExecCapacity,
    NativeMutationStateUnknown,
    UnknownResult(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NativeFailure {
    pub(super) status: i32,
    pub(super) detail: String,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchedulerStateUnavailable => formatter.write_str("mutation scheduler state is unavailable"),
            Self::ScheduleFailed(status) => {
                write!(
                    formatter,
                    "AutoCAD could not schedule the operation (status {status})"
                )
            }
            Self::Stopped => formatter.write_str("the native operation stopped before completion"),
            Self::PluginStopping => formatter.write_str("the acadctl plugin is stopping"),
            Self::DrawingNotFound(id) => write!(formatter, "Drawing '{id}' is not open."),
            Self::DrawingGone => formatter.write_str("The drawing is no longer open"),
            Self::DrawingGenerationChanged => formatter
                .write_str("The drawing was replaced before AutoCAD could perform the operation"),
            Self::Unnamed(id) => write!(
                formatter,
                "Drawing '{id}' has no file name. Save As is not supported yet."
            ),
            Self::ReadOnly(id) => write!(formatter, "Drawing '{id}' is read-only."),
            Self::Dirty(id) => write!(
                formatter,
                "Drawing '{id}' has unsaved changes."
            ),
            Self::NotDwg => formatter.write_str("Only DWG drawings can be saved"),
            Self::OpenFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not open the drawing")
            }
            Self::LockFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not lock the drawing")
            }
            Self::SaveFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not save the drawing")
            }
            Self::CloseFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not close the drawing")
            }
            Self::HistoryFailed { direction, failure } => failure.fmt_with_context(
                formatter,
                match direction {
                    HistoryDirection::Undo => "Could not undo the drawing's last history step",
                    HistoryDirection::Redo => "Could not redo the drawing's next history step",
                },
            ),
            Self::OpenNotPublished => formatter
                .write_str("AutoCAD opened the drawing but did not publish its drawing state"),
            Self::SaveNotPublished => {
                formatter.write_str("AutoCAD completed the save but still reports unsaved changes")
            }
            Self::CloseNotPublished => {
                formatter.write_str("AutoCAD completed the close but the drawing is still open")
            }
            Self::NotQuiescent => formatter.write_str("The drawing is busy"),
            Self::UndoDisabled => {
                formatter.write_str("Undo recording is disabled for the drawing")
            }
            Self::DocumentContextFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not establish AutoCAD document context")
            }
            Self::DocumentContextRestoreFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release AutoCAD document context safely",
            ),
            Self::ExecBridgeFinalizationFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release native execution state safely",
            ),
            Self::ExecBridgeSymbolsClearFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not clear the reserved AutoLISP execution bridge symbols",
            ),
            Self::ExecBridgeFailed(failure) => {
                failure.fmt_with_context(formatter, "The AutoLISP execution bridge failed")
            }
            Self::ExecNotFinished => {
                formatter.write_str("The native execution ended without a terminal outcome")
            }
            Self::MutationCapacity => {
                formatter.write_str("AutoCAD already has the maximum number of pending operations")
            }
            Self::ExecCapacity => formatter.write_str(
                "AutoCAD already has the maximum number or total size of execution requests",
            ),
            Self::NativeMutationStateUnknown => formatter.write_str(
                "AutoCAD's mutation context is unknown; restart AutoCAD before issuing another mutation",
            ),
            Self::UnknownResult(kind) => {
                write!(
                    formatter,
                    "AutoCAD returned an unknown native result ({kind})"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl NativeFailure {
    fn fmt_with_context(&self, formatter: &mut fmt::Formatter<'_>, context: &str) -> fmt::Result {
        if self.detail.is_empty() {
            write!(formatter, "{context} (ObjectARX status {})", self.status)
        } else {
            write!(formatter, "{context}: {}", self.detail)
        }
    }
}

impl Error {
    pub const fn drawing_error_kind(&self) -> Option<DrawingErrorKind> {
        match self {
            Self::DrawingNotFound(_) | Self::DrawingGone => Some(DrawingErrorKind::NotOpen),
            Self::DrawingGenerationChanged => Some(DrawingErrorKind::Replaced),
            Self::Unnamed(_) => Some(DrawingErrorKind::NoFileName),
            Self::ReadOnly(_) => Some(DrawingErrorKind::ReadOnly),
            Self::Dirty(_) => Some(DrawingErrorKind::UnsavedChanges),
            Self::NotQuiescent => Some(DrawingErrorKind::Busy),
            Self::UndoDisabled => Some(DrawingErrorKind::UndoDisabled),
            _ => None,
        }
    }

    pub const fn drawing_id(&self) -> Option<DrawingId> {
        match self {
            Self::DrawingNotFound(id)
            | Self::Unnamed(id)
            | Self::ReadOnly(id)
            | Self::Dirty(id) => Some(*id),
            _ => None,
        }
    }

    pub const fn is_internal(&self) -> bool {
        matches!(
            self,
            Self::SchedulerStateUnavailable
                | Self::Stopped
                | Self::OpenNotPublished
                | Self::SaveNotPublished
                | Self::CloseNotPublished
                | Self::DocumentContextFailed(_)
                | Self::DocumentContextRestoreFailed(_)
                | Self::ExecBridgeFinalizationFailed(_)
                | Self::ExecBridgeSymbolsClearFailed(_)
                | Self::ExecBridgeFailed(_)
                | Self::ExecNotFinished
                | Self::NativeMutationStateUnknown
        )
    }

    pub const fn drawing_outcome(&self) -> DrawingOutcome {
        if matches!(
            self,
            Self::DocumentContextRestoreFailed(_)
                | Self::ExecBridgeFinalizationFailed(_)
                | Self::ExecBridgeSymbolsClearFailed(_)
                | Self::ExecBridgeFailed(_)
                | Self::ExecNotFinished
                | Self::NativeMutationStateUnknown
                | Self::Stopped
                | Self::UnknownResult(_)
        ) {
            DrawingOutcome::Unknown
        } else {
            DrawingOutcome::NotStarted
        }
    }
}
