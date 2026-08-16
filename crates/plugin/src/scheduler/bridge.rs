use std::time::Instant;

use crate::doc::NativeDocKey;
use crate::exec::value::writer::NativeValueWriter;
use crate::exec::{ExecStepResult, NativeExecStep, bound_diagnostic};

use super::operation::Operation;
use super::queue::{MutationJobId, SCHEDULER};

pub(crate) fn take_execution_step(job_id: MutationJobId) -> NativeExecStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecStep::invalid();
    };

    let Some(job) = scheduler.active.as_mut() else {
        return NativeExecStep::invalid();
    };

    if job.job_id != job_id {
        return NativeExecStep::invalid();
    }

    job.expire_if_due(Instant::now());

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.take_step(),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => NativeExecStep::invalid(),
    }
}

pub(crate) fn begin_eval_value(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> NativeValueWriter {
    let lease = {
        let Ok(scheduler) = SCHEDULER.lock() else {
            return NativeValueWriter::inactive();
        };

        let Some(job) = scheduler.active.as_ref() else {
            return NativeValueWriter::inactive();
        };

        if job.job_id != job_id
            || job.native_target
                != Some(NativeDocKey {
                    document_token,
                    database_token,
                })
        {
            return NativeValueWriter::inactive();
        }

        match &job.operation {
            Operation::Execute { execution, .. } => execution.acquire_eval_value_output(),
            Operation::Open { .. }
            | Operation::Save { .. }
            | Operation::Close { .. }
            | Operation::History { .. } => None,
        }
    };

    lease.map_or_else(NativeValueWriter::inactive, NativeValueWriter::eval_value)
}

pub(crate) fn complete_execution_step(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };

    if job.job_id != job_id {
        return false;
    }

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.complete_step(result),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => false,
    }
}

pub(crate) fn abandon_execution(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };

    if job.job_id != job_id {
        return false;
    }

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.abandon(result),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => false,
    }
}
