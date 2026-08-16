use super::output::{EmitResult, OutputSink};

pub(crate) const MAX_LABEL_BYTES: usize = 4 * 1024;

pub(crate) fn write(sink: &OutputSink, text: &str) -> EmitResult {
    if text.is_empty() || text.chars().any(char::is_control) {
        EmitResult::InvalidSequence
    } else if text.len() > MAX_LABEL_BYTES {
        EmitResult::LimitExceeded
    } else {
        emit_label(sink, text)
    }
}

fn emit_label(sink: &super::output::OutputSink, text: &str) -> EmitResult {
    for fragment in ["--- ", text, " ---\n"] {
        let result = sink.emit(fragment);

        if result != EmitResult::Continue {
            return result;
        }
    }

    sink.flush()
}

#[cfg(test)]
mod tests {
    use super::super::output::{OutputStream, channel};
    use super::*;

    #[tokio::test]
    async fn renders_one_distinct_line() {
        let (sink, stream) = channel();
        let terminal = sink.clone();

        assert_eq!(write(&sink, "layers before"), EmitResult::Continue);
        terminal.finish();

        assert_eq!(collect(stream).await, "--- layers before ---\n");
    }

    #[test]
    fn rejects_non_strings_multiline_text_and_oversized_text() {
        for text in ["two\nlines", ""] {
            let (sink, _stream) = channel();
            assert_eq!(write(&sink, text), EmitResult::InvalidSequence);
        }

        let (sink, _stream) = channel();
        assert_eq!(
            write(&sink, &"x".repeat(MAX_LABEL_BYTES + 1)),
            EmitResult::LimitExceeded
        );
    }

    async fn collect(stream: OutputStream) -> String {
        let mut output = String::new();

        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }

        output
    }
}
