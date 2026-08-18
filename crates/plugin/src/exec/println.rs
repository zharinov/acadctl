use super::output::{EmitResult, OutputSink};

pub(crate) const MAX_PRINTLN_BYTES: usize = 4 * 1024;

pub(crate) fn write(sink: &OutputSink, text: &str) -> EmitResult {
    if text.chars().any(char::is_control) {
        EmitResult::InvalidSequence
    } else if text.len() > MAX_PRINTLN_BYTES {
        EmitResult::LimitExceeded
    } else {
        emit_line(sink, text)
    }
}

fn emit_line(sink: &OutputSink, text: &str) -> EmitResult {
    for fragment in [text, "\n"] {
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
    async fn prints_the_string_and_a_newline() {
        let (sink, stream) = channel();
        let terminal = sink.clone();

        assert_eq!(write(&sink, "layers before"), EmitResult::Continue);
        assert_eq!(write(&sink, ""), EmitResult::Continue);
        terminal.finish();

        assert_eq!(collect(stream).await, "layers before\n\n");
    }

    #[test]
    fn rejects_control_characters_and_oversized_text() {
        let (sink, _stream) = channel();
        assert_eq!(write(&sink, "two\nlines"), EmitResult::InvalidSequence);

        let (sink, _stream) = channel();
        assert_eq!(
            write(&sink, &"x".repeat(MAX_PRINTLN_BYTES + 1)),
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
