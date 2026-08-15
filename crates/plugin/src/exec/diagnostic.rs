const DIAGNOSTIC_TRUNCATION_SUFFIX: &str = "... [truncated]";

pub(crate) fn bound_diagnostic(message: &mut String) {
    bound_diagnostic_with_native_truncation(message, false);
}

pub(super) fn bound_diagnostic_with_native_truncation(
    message: &mut String,
    native_truncated: bool,
) {
    if !native_truncated && message.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES {
        return;
    }

    let mut end = message
        .len()
        .min(acadctl_rpc::MAX_DIAGNOSTIC_BYTES - DIAGNOSTIC_TRUNCATION_SUFFIX.len());

    while !message.is_char_boundary(end) {
        end -= 1;
    }

    message.truncate(end);
    message.push_str(DIAGNOSTIC_TRUNCATION_SUFFIX);
}

pub(crate) fn bounded_diagnostic(mut message: String) -> String {
    bound_diagnostic(&mut message);
    message
}

pub(crate) fn bounded_native_diagnostic(mut message: String, native_truncated: bool) -> String {
    bound_diagnostic_with_native_truncation(&mut message, native_truncated);
    message
}

pub(super) fn append_diagnostic(message: &mut String, detail: &str) {
    message.push_str("; ");
    message.push_str(detail);
    bound_diagnostic(message);
}
