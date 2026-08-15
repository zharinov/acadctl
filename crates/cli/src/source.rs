use std::io::{self, IsTerminal, Read};
use std::path::Path;

use bytes::Bytes;

const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
const READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_RAW_SOURCE_BYTES: usize = acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + UTF8_BOM.len() + 1;

#[derive(Debug)]
pub struct SourceInput {
    pub name: String,
    pub bytes: Bytes,
}

#[derive(Debug)]
pub enum SourceError {
    Message(String),
    Scan {
        source_name: String,
        error: acadctl_lisp::ScanError,
    },
}

pub fn read(file: Option<&Path>, eval: bool) -> Result<SourceInput, SourceError> {
    match file {
        None => read_stdin(eval),
        Some(path) if path == Path::new("-") => read_stdin(eval),
        Some(path) => read_file(path, eval),
    }
}

impl SourceError {
    pub fn report(&self) {
        match self {
            Self::Message(message) => eprintln!("acadctl: {message}"),
            Self::Scan { source_name, error } => {
                eprintln!(
                    "Read error in {source_name} (line {}, column {}).",
                    error.line, error.column
                );
                eprintln!("{}", error.kind.message());
            }
        }
    }
}

fn read_stdin(eval: bool) -> Result<SourceInput, SourceError> {
    let stdin = io::stdin();

    let bytes = read_bounded(stdin.lock()).map_err(|error| {
        SourceError::Message(format!("Could not read AutoLISP from stdin: {error}"))
    })?;
    validate("<stdin>".into(), bytes, eval)
}

fn read_file(path: &Path, eval: bool) -> Result<SourceInput, SourceError> {
    let source_name = path.to_str().map(str::to_owned).ok_or_else(|| {
        SourceError::Message(format!(
            "Source path '{}' is not valid UTF-8.",
            path.to_string_lossy()
        ))
    })?;

    if source_name.len() > acadctl_rpc::MAX_SOURCE_NAME_BYTES {
        return Err(SourceError::Message(
            "The source path exceeds the 4 KiB limit.".into(),
        ));
    }

    let file = std::fs::File::open(path).map_err(|error| {
        SourceError::Message(format!("Could not read '{source_name}': {error}"))
    })?;
    let bytes = read_bounded(file).map_err(|error| {
        SourceError::Message(format!("Could not read '{source_name}': {error}"))
    })?;
    validate(source_name, bytes, eval)
}

fn read_bounded(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; READ_BUFFER_BYTES];

    while bytes.len() < MAX_RAW_SOURCE_BYTES {
        let remaining = MAX_RAW_SOURCE_BYTES - bytes.len();
        let buffer_length = remaining.min(buffer.len());
        let count = match reader.read(&mut buffer[..buffer_length]) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };

        let needed = bytes.len() + count;

        if needed > bytes.capacity() {
            let capacity = if bytes.capacity() == 0 {
                READ_BUFFER_BYTES
            } else {
                bytes.capacity().saturating_mul(2)
            }
            .max(needed)
            .min(MAX_RAW_SOURCE_BYTES);
            bytes
                .try_reserve_exact(capacity - bytes.len())
                .map_err(|_| io::Error::other("could not allocate the source buffer"))?;
        }

        bytes.extend_from_slice(&buffer[..count]);
    }

    Ok(bytes)
}

fn validate(source_name: String, bytes: Vec<u8>, eval: bool) -> Result<SourceInput, SourceError> {
    let mut bytes = Bytes::from(bytes);

    if bytes.starts_with(UTF8_BOM) {
        bytes = bytes.slice(UTF8_BOM.len()..);
    }

    if bytes.len() > acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES {
        return Err(SourceError::Message(
            "The source exceeds the 4 MiB limit.".into(),
        ));
    }

    let source = std::str::from_utf8(&bytes)
        .map_err(|_| SourceError::Message("The source is not valid UTF-8.".into()))?;

    if source.contains('\0') {
        return Err(SourceError::Message(
            "The source contains U+0000, which AutoLISP cannot represent.".into(),
        ));
    }

    let form_count = acadctl_lisp::validate(source).map_err(|error| SourceError::Scan {
        source_name: source_name.clone(),
        error,
    })?;

    if eval && form_count != 1 {
        return Err(SourceError::Message(format!(
            "eval requires exactly one top-level form; found {form_count}."
        )));
    }

    Ok(SourceInput {
        name: source_name,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_bom_and_accepts_the_exact_source_limit() {
        let mut source = Vec::with_capacity(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3);
        source.extend_from_slice(UTF8_BOM);
        source.resize(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 3, b'x');

        let source = validate("script.lsp".into(), source, false).unwrap();

        assert_eq!(source.bytes.len(), acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES);
        assert_eq!(source.bytes[0], b'x');
    }

    #[test]
    fn rejects_oversize_invalid_and_unrepresentable_source() {
        assert!(matches!(
            validate(
                "script.lsp".into(),
                vec![b'x'; acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 1],
                false
            ),
            Err(SourceError::Message(message)) if message.contains("4 MiB")
        ));
        assert!(matches!(
            validate("script.lsp".into(), vec![0xff], false),
            Err(SourceError::Message(message)) if message.contains("UTF-8")
        ));
        assert!(matches!(
            validate("script.lsp".into(), b"a\0b".to_vec(), false),
            Err(SourceError::Message(message)) if message.contains("U+0000")
        ));
    }

    #[test]
    fn applies_eval_shape_and_reports_scanner_locations() {
        assert!(matches!(
            validate("script.lsp".into(), b"(a) (b)".to_vec(), true),
            Err(SourceError::Message(message)) if message.contains("found 2")
        ));
        assert!(matches!(
            validate("script.lsp".into(), b"\n  (a".to_vec(), false),
            Err(SourceError::Scan { source_name, error })
                if source_name == "script.lsp" && error.line == 2 && error.column == 3
        ));
        assert!(validate("script.lsp".into(), Vec::new(), false).is_ok());
    }

    #[test]
    fn bounded_reader_never_retains_more_than_the_probe_limit() {
        let bytes = read_bounded(io::repeat(b'x')).unwrap();

        assert_eq!(bytes.len(), MAX_RAW_SOURCE_BYTES);
        assert!(bytes.capacity() <= MAX_RAW_SOURCE_BYTES);
    }

    #[test]
    fn bounded_reader_does_not_overallocate_after_short_reads() {
        struct ShortReader {
            reads: usize,
        }

        impl Read for ShortReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = if self.reads < 63 {
                    buffer.len()
                } else if self.reads == 63 {
                    buffer.len().min(READ_BUFFER_BYTES - 1)
                } else {
                    buffer.len()
                };

                buffer[..count].fill(b'x');
                self.reads += 1;
                Ok(count)
            }
        }

        let bytes = read_bounded(ShortReader { reads: 0 }).unwrap();

        assert_eq!(bytes.len(), MAX_RAW_SOURCE_BYTES);
        assert!(bytes.capacity() <= MAX_RAW_SOURCE_BYTES);
    }
}
