use super::super::output::{EmitResult, OUTPUT_CHUNK_BYTES, OutputSink};

pub const MAX_VALUE_DEPTH: usize = 64 * 1024;
pub const MAX_VALUE_TEXT_BYTES: usize = OUTPUT_CHUNK_BYTES;

const MAX_ENTITY_HANDLE_BYTES: usize = 32;
const MAX_OBJECT_LABEL_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintError {
    InvalidSequence,
    LimitExceeded,
    Output(EmitResult),
}

pub struct ValuePrinter {
    sink: OutputSink,
    lists: Vec<ListState>,
    atom: AtomState,
    skipped_lists: usize,
    root_values: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ListState {
    Empty,
    Values,
    AwaitingTail,
    Complete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtomState {
    None,
    String,
    Symbol(SymbolState),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SymbolState {
    number: NumberState,
    nil: NilState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberState {
    Start,
    SignOnly,
    IntegerDigits,
    ExponentMarker,
    ExponentSign,
    ExponentDigits,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NilState {
    Start,
    N,
    Ni,
    Nil,
    Other,
}

#[derive(Clone, Copy)]
enum OpaqueKind {
    Entity,
    SelectionSet,
    VlaObject,
    File,
    Function,
    Error,
    Object,
    Cycle,
    TooDeep,
}

impl ValuePrinter {
    pub fn new(sink: OutputSink) -> Self {
        Self {
            sink,
            lists: Vec::new(),
            atom: AtomState::None,
            skipped_lists: 0,
            root_values: 0,
        }
    }

    pub const fn root_values(&self) -> usize {
        self.root_values
    }

    pub fn begin_list(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            self.poll_output()?;
            self.skipped_lists = self
                .skipped_lists
                .checked_add(1)
                .ok_or(PrintError::LimitExceeded)?;

            return Ok(());
        }

        self.require_no_atom()?;

        if self.lists.len() == MAX_VALUE_DEPTH {
            self.too_deep()?;
            self.skipped_lists = 1;

            return Ok(());
        }

        self.before_value()?;
        self.write("(")?;
        self.lists
            .try_reserve(1)
            .map_err(|_| PrintError::LimitExceeded)?;
        self.lists.push(ListState::Empty);
        Ok(())
    }

    pub fn end_list(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            self.poll_output()?;
            self.skipped_lists -= 1;

            return Ok(());
        }

        self.require_no_atom()?;
        let Some(state) = self.lists.last() else {
            return Err(PrintError::InvalidSequence);
        };

        if *state == ListState::AwaitingTail {
            return Err(PrintError::InvalidSequence);
        }

        self.lists.pop();
        self.write(")")
    }

    pub fn dot(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        let Some(state) = self.lists.last_mut() else {
            return Err(PrintError::InvalidSequence);
        };

        if *state != ListState::Values {
            return Err(PrintError::InvalidSequence);
        }

        *state = ListState::AwaitingTail;
        self.write(" . ")
    }

    pub fn nil(&mut self) -> Result<(), PrintError> {
        self.scalar("nil")
    }

    pub fn true_value(&mut self) -> Result<(), PrintError> {
        self.scalar("T")
    }

    pub fn integer(&mut self, value: i64) -> Result<(), PrintError> {
        self.scalar(&value.to_string())
    }

    pub fn real(&mut self, value: f64) -> Result<(), PrintError> {
        let text = format_autolisp_real(value).ok_or(PrintError::InvalidSequence)?;
        self.scalar(&text)
    }

    pub fn begin_symbol(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        self.poll_output()?;
        self.atom = AtomState::Symbol(SymbolState::new());
        Ok(())
    }

    pub fn symbol_chunk(&mut self, text: &str) -> Result<(), PrintError> {
        if text.len() > MAX_VALUE_TEXT_BYTES || !text.chars().all(valid_symbol_character) {
            return Err(PrintError::InvalidSequence);
        }

        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        match self.atom {
            AtomState::Symbol(_) if text.is_empty() => self.poll_output(),
            AtomState::Symbol(mut state) => {
                if state.is_empty() {
                    self.before_value()?;
                }

                state.push(text);
                self.atom = AtomState::Symbol(state);
                self.write_symbol_chunk(text)
            }
            AtomState::None | AtomState::String => Err(PrintError::InvalidSequence),
        }
    }

    pub fn end_symbol(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        let AtomState::Symbol(state) = self.atom else {
            return Err(PrintError::InvalidSequence);
        };

        if !state.is_valid() {
            return Err(PrintError::InvalidSequence);
        }

        self.poll_output()?;
        self.atom = AtomState::None;
        Ok(())
    }

    pub fn begin_string(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        self.poll_output()?;
        self.before_value()?;

        self.write("\"")?;

        self.atom = AtomState::String;
        Ok(())
    }

    pub fn string_chunk(&self, text: &str) -> Result<(), PrintError> {
        if text.len() > MAX_VALUE_TEXT_BYTES {
            return Err(PrintError::InvalidSequence);
        }

        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        if self.atom != AtomState::String {
            return Err(PrintError::InvalidSequence);
        }

        if !text.chars().any(requires_readable_escape) {
            return self.write(text);
        }

        let mut escaped = String::new();

        for character in text.chars() {
            match character {
                '"' => self.append_bounded(&mut escaped, "\\\"")?,
                '\\' => self.append_bounded(&mut escaped, "\\\\")?,
                '\u{1b}' => self.append_bounded(&mut escaped, "\\e")?,
                '\n' => self.append_bounded(&mut escaped, "\\n")?,
                '\r' => self.append_bounded(&mut escaped, "\\r")?,
                '\t' => self.append_bounded(&mut escaped, "\\t")?,
                character if character.is_control() => {
                    let value = character as u32;
                    let octal = [
                        b'\\',
                        b'0' + ((value >> 6) & 7) as u8,
                        b'0' + ((value >> 3) & 7) as u8,
                        b'0' + (value & 7) as u8,
                    ];
                    self.append_bounded(
                        &mut escaped,
                        std::str::from_utf8(&octal).expect("octal escape is ASCII"),
                    )?;
                }
                character => {
                    let mut encoded = [0; 4];
                    self.append_bounded(&mut escaped, character.encode_utf8(&mut encoded))?;
                }
            }
        }

        self.write(&escaped)
    }

    pub fn end_string(&mut self) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        if self.atom != AtomState::String {
            return Err(PrintError::InvalidSequence);
        }

        self.poll_output()?;
        self.atom = AtomState::None;

        self.write("\"")?;

        Ok(())
    }

    pub fn entity(&mut self, handle: Option<&str>) -> Result<(), PrintError> {
        let handle = handle
            .filter(|handle| {
                !handle.is_empty()
                    && handle.len() <= MAX_ENTITY_HANDLE_BYTES
                    && handle.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map(str::to_ascii_uppercase);
        let Some(handle) = handle else {
            return self.opaque_value(OpaqueKind::Entity, None);
        };

        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        self.before_value()?;
        self.write("(handent \"")?;
        self.write(&handle)?;
        self.write("\")")
    }

    pub fn selection_set(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::SelectionSet, None)
    }

    pub fn vla_object(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::VlaObject, None)
    }

    pub fn file(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::File, None)
    }

    pub fn function(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::Function, None)
    }

    pub fn error_object(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::Error, None)
    }

    pub fn object(&mut self, label: Option<&str>) -> Result<(), PrintError> {
        self.opaque_value(
            OpaqueKind::Object,
            label.filter(|label| valid_label(label, MAX_OBJECT_LABEL_BYTES)),
        )
    }

    pub fn cycle(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::Cycle, None)
    }

    pub fn too_deep(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::TooDeep, None)
    }

    pub fn finish(self) -> Result<(), PrintError> {
        if self.atom != AtomState::None || !self.lists.is_empty() || self.skipped_lists != 0 {
            let _ = self.sink.flush();

            return Err(PrintError::InvalidSequence);
        }

        self.write("\n")?;

        match self.sink.flush() {
            EmitResult::Continue => Ok(()),
            result => Err(PrintError::Output(result)),
        }
    }

    fn scalar(&mut self, text: &str) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        self.before_value()?;
        self.write(text)
    }

    fn opaque_value(&mut self, kind: OpaqueKind, payload: Option<&str>) -> Result<(), PrintError> {
        if self.skipped_lists != 0 {
            return self.poll_output();
        }

        self.require_no_atom()?;
        self.before_value()?;
        self.write("#<")?;
        self.write(kind.name())?;

        if let Some(payload) = payload.filter(|payload| !payload.is_empty()) {
            self.write(" ")?;
            self.write(payload)?;
        }

        self.write(">")
    }

    fn before_value(&mut self) -> Result<(), PrintError> {
        let Some(state) = self.lists.last_mut() else {
            self.root_values = self
                .root_values
                .checked_add(1)
                .ok_or(PrintError::LimitExceeded)?;

            return Ok(());
        };

        let write_space = match *state {
            ListState::Empty => {
                *state = ListState::Values;
                false
            }
            ListState::Values => true,
            ListState::AwaitingTail => {
                *state = ListState::Complete;
                false
            }
            ListState::Complete => return Err(PrintError::InvalidSequence),
        };

        if write_space {
            self.write(" ")?;
        }

        Ok(())
    }

    fn require_no_atom(&self) -> Result<(), PrintError> {
        if self.atom == AtomState::None {
            Ok(())
        } else {
            Err(PrintError::InvalidSequence)
        }
    }

    fn write(&self, text: &str) -> Result<(), PrintError> {
        match self.sink.emit(text) {
            EmitResult::Continue => Ok(()),
            result => Err(PrintError::Output(result)),
        }
    }

    fn poll_output(&self) -> Result<(), PrintError> {
        self.write("")
    }

    fn append_bounded(&self, buffer: &mut String, text: &str) -> Result<(), PrintError> {
        if buffer.len() + text.len() > OUTPUT_CHUNK_BYTES {
            self.write(buffer)?;
            buffer.clear();
        }

        buffer.push_str(text);
        Ok(())
    }

    fn write_symbol_chunk(&self, text: &str) -> Result<(), PrintError> {
        if !text.contains('\\') {
            return self.write(text);
        }

        let mut escaped = String::new();

        for fragment in text.split_inclusive('\\') {
            if let Some(prefix) = fragment.strip_suffix('\\') {
                self.append_bounded(&mut escaped, prefix)?;
                self.append_bounded(&mut escaped, "\\\\")?;
            } else {
                self.append_bounded(&mut escaped, fragment)?;
            }
        }

        self.write(&escaped)
    }
}

impl SymbolState {
    const fn new() -> Self {
        Self {
            number: NumberState::Start,
            nil: NilState::Start,
        }
    }

    fn is_empty(self) -> bool {
        self.number == NumberState::Start
    }

    fn is_valid(self) -> bool {
        !matches!(
            self.number,
            NumberState::Start | NumberState::IntegerDigits | NumberState::ExponentDigits
        ) && self.nil != NilState::Nil
    }

    fn push(&mut self, text: &str) {
        for character in text.chars() {
            self.number = self.number.advance(character);
            self.nil = self.nil.advance(character);
        }
    }
}

impl NumberState {
    fn advance(self, character: char) -> Self {
        match (self, character) {
            (Self::Start, '+' | '-') => Self::SignOnly,
            (Self::Start | Self::SignOnly, character) if character.is_ascii_digit() => {
                Self::IntegerDigits
            }
            (Self::IntegerDigits, character) if character.is_ascii_digit() => Self::IntegerDigits,
            (Self::IntegerDigits, 'e' | 'E') => Self::ExponentMarker,
            (Self::ExponentMarker, '+' | '-') => Self::ExponentSign,
            (Self::ExponentMarker | Self::ExponentSign, character)
                if character.is_ascii_digit() =>
            {
                Self::ExponentDigits
            }
            (Self::ExponentDigits, character) if character.is_ascii_digit() => Self::ExponentDigits,
            (Self::Other, _) => Self::Other,
            _ => Self::Other,
        }
    }
}

impl NilState {
    fn advance(self, character: char) -> Self {
        match (self, character.to_ascii_uppercase()) {
            (Self::Start, 'N') => Self::N,
            (Self::N, 'I') => Self::Ni,
            (Self::Ni, 'L') => Self::Nil,
            (Self::Other, _) => Self::Other,
            _ => Self::Other,
        }
    }
}

impl OpaqueKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Entity => "Entity",
            Self::SelectionSet => "SelectionSet",
            Self::VlaObject => "VlaObject",
            Self::File => "File",
            Self::Function => "Function",
            Self::Error => "Error",
            Self::Object => "Object",
            Self::Cycle => "Cycle",
            Self::TooDeep => "TooDeep",
        }
    }
}

fn format_autolisp_real(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }

    if value == 0.0 {
        return Some("0.0".to_owned());
    }

    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e')?;
    let exponent = exponent.parse::<i32>().ok()?;
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(char::from)
        .collect();

    if (-4..6).contains(&exponent) {
        let decimal = exponent + 1;
        let mut result = String::with_capacity(16);

        if negative {
            result.push('-');
        }

        if decimal <= 0 {
            result.push_str("0.");
            result.extend(std::iter::repeat_n('0', decimal.unsigned_abs() as usize));
            result.push_str(&digits);
        } else {
            let decimal = usize::try_from(decimal).ok()?;

            if decimal >= digits.len() {
                result.push_str(&digits);
                result.extend(std::iter::repeat_n('0', decimal - digits.len()));
                result.push_str(".0");

                return Some(result);
            }

            result.push_str(&digits[..decimal]);
            result.push('.');
            result.push_str(&digits[decimal..]);
        }

        trim_fraction(&mut result);
        Some(result)
    } else {
        let mut result = mantissa.to_owned();
        trim_fraction(&mut result);
        result.push('e');

        if exponent >= 0 {
            result.push('+');
        } else {
            result.push('-');
        }

        let magnitude = exponent.unsigned_abs();

        if magnitude < 10 {
            result.push('0');
        }

        result.push_str(&magnitude.to_string());
        Some(result)
    }
}

fn trim_fraction(value: &mut String) {
    while value.ends_with('0') && !value.ends_with(".0") {
        value.pop();
    }
}

fn valid_label(text: &str, max_bytes: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_bytes
        && text.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '<' | '>' | '@' | '"' | '\\')
        })
}

fn valid_symbol_character(character: char) -> bool {
    !character.is_control()
        && !character.is_whitespace()
        && !matches!(character, '(' | ')' | '"' | ';' | '\'' | '.')
}

fn requires_readable_escape(character: char) -> bool {
    matches!(character, '"' | '\\' | '\u{1b}' | '\n' | '\r' | '\t') || character.is_control()
}

#[cfg(test)]
#[path = "printer_fixtures.rs"]
mod fixture_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::output::{OutputStream, channel};

    #[test]
    fn rejects_numeric_and_nil_symbols_across_chunks() {
        for chunks in [
            &["123"][..],
            &["+", "123"][..],
            &["-1", "23"][..],
            &["1", "e", "3"][..],
            &["1e", "-", "3"][..],
            &["n", "I", "l"][..],
        ] {
            let (sink, _stream) = channel();
            let mut printer = ValuePrinter::new(sink);
            printer.begin_symbol().unwrap();

            for chunk in chunks {
                printer.symbol_chunk(chunk).unwrap();
            }

            assert_eq!(printer.end_symbol(), Err(PrintError::InvalidSequence));
        }
    }

    #[tokio::test]
    async fn normalizes_or_rejects_opaque_payloads() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink);
        printer.begin_list().unwrap();
        printer.entity(Some("5a2")).unwrap();
        printer.entity(Some("not-a-handle")).unwrap();
        printer.object(Some("bad label")).unwrap();
        printer.end_list().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(
            collect(stream).await,
            "((handent \"5A2\") #<Entity> #<Object>)\n"
        );
    }

    #[test]
    fn matches_live_autolisp_real_rounding() {
        for (value, expected) in [
            (9.999994, "9.99999"),
            (9.999995, "10.0"),
            (999_999.4, "999999.0"),
            (999_999.5, "1.0e+06"),
            (0.000_099_999_94, "9.99999e-05"),
            (0.000_099_999_95, "0.0001"),
            (999_996.5, "999996.0"),
            (999_997.5, "999998.0"),
            (999_998.5, "999998.0"),
            (100_000.5, "100000.0"),
        ] {
            assert_eq!(format_autolisp_real(value).as_deref(), Some(expected));
            let negative = format!("-{expected}");
            assert_eq!(
                format_autolisp_real(-value).as_deref(),
                Some(negative.as_str())
            );
        }

        assert_eq!(
            format_autolisp_real(f64::MAX).as_deref(),
            Some("1.79769e+308")
        );
        assert_eq!(
            format_autolisp_real(f64::MIN_POSITIVE).as_deref(),
            Some("2.22507e-308")
        );
        assert_eq!(format_autolisp_real(-0.0).as_deref(), Some("0.0"));
        assert_eq!(format_autolisp_real(f64::NAN), None);
        assert_eq!(format_autolisp_real(f64::INFINITY), None);
    }

    #[test]
    fn rejects_malformed_or_oversized_events() {
        let (sink, _stream) = channel();
        let mut printer = ValuePrinter::new(sink);
        assert_eq!(printer.dot(), Err(PrintError::InvalidSequence));
        assert_eq!(
            printer.string_chunk("not begun"),
            Err(PrintError::InvalidSequence)
        );
        printer.begin_symbol().unwrap();
        assert_eq!(printer.end_symbol(), Err(PrintError::InvalidSequence));
        assert_eq!(
            printer.symbol_chunk("not a symbol"),
            Err(PrintError::InvalidSequence)
        );
        assert_eq!(
            printer.symbol_chunk(&"x".repeat(MAX_VALUE_TEXT_BYTES + 1)),
            Err(PrintError::InvalidSequence)
        );
    }

    #[test]
    fn observes_cancellation_while_skipping_excessive_depth() {
        let (sink, _stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink);

        for _ in 0..=MAX_VALUE_DEPTH {
            printer.begin_list().unwrap();
        }

        terminal.request_cancel();

        assert_eq!(
            printer.integer(1),
            Err(PrintError::Output(EmitResult::Cancelled))
        );
    }

    #[tokio::test]
    async fn replaces_excessive_depth_with_too_deep() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink);

        for _ in 0..=MAX_VALUE_DEPTH {
            printer.begin_list().unwrap();
        }

        printer.integer(1).unwrap();

        for _ in 0..=MAX_VALUE_DEPTH {
            printer.end_list().unwrap();
        }

        printer.finish().unwrap();
        terminal.finish();

        let output = collect(stream).await;
        assert!(output.contains("#<TooDeep>"));
        assert!(!output.contains('1'));
    }

    async fn collect(stream: OutputStream) -> String {
        let mut output = String::new();

        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }

        output
    }
}
