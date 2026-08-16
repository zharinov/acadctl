use std::time::Instant;

use crate::drawing::NativeDocumentKey;
use crate::exec::value::port::NativeOutputPort;
use crate::exec::{ExecStepResult, NativeExecStep, bound_diagnostic};

use super::queue::{MutationJobId, SCHEDULER};

pub(crate) fn take_execution_step(job_id: MutationJobId) -> NativeExecStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecStep::invalid();
    };

    scheduler.take_execution_step(job_id, Instant::now())
}

pub(crate) fn begin_eval_output(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> NativeOutputPort {
    let Ok(scheduler) = SCHEDULER.lock() else {
        return NativeOutputPort::inactive();
    };

    let lease = scheduler.acquire_eval_value_output(
        job_id,
        NativeDocumentKey {
            document_token,
            database_token,
        },
    );

    lease.map_or_else(NativeOutputPort::inactive, NativeOutputPort::eval_value)
}

pub(crate) fn begin_form_output(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> NativeOutputPort {
    form_output_lease(job_id, document_token, database_token)
        .map_or_else(NativeOutputPort::inactive, NativeOutputPort::form)
}

fn form_output_lease(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> Option<crate::exec::ValueOutputLease> {
    let Ok(scheduler) = SCHEDULER.lock() else {
        return None;
    };

    scheduler.acquire_form_output(
        job_id,
        NativeDocumentKey {
            document_token,
            database_token,
        },
    )
}

pub(crate) fn complete_execution_step(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    scheduler.complete_execution_step(job_id, result)
}

pub(crate) fn abandon_execution(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    scheduler.abandon_execution(job_id, result)
}
