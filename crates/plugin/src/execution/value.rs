#![allow(
    dead_code,
    reason = "the value printer stays private until the native output bridge is connected"
)]

use super::output::{EmitResult, OUTPUT_CHUNK_BYTES, OutputSink};

pub const MAX_VALUE_DEPTH: usize = 64 * 1024;
pub const MAX_VALUE_TEXT_BYTES: usize = OUTPUT_CHUNK_BYTES;

const MAX_REAL_TEXT_BYTES: usize = 64;
const MAX_ENTITY_HANDLE_BYTES: usize = 32;
const MAX_CLASS_NAME_BYTES: usize = 128;
const MAX_FUNCTION_NAME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintMode {
    Display,
    Readable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintError {
    InvalidSequence,
    LimitExceeded,
    Output(EmitResult),
}

pub struct ValuePrinter {
    sink: OutputSink,
    mode: PrintMode,
    lists: Vec<ListState>,
    atom: AtomState,
    skipped_lists: usize,
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
    Void,
}

impl ValuePrinter {
    pub fn new(sink: OutputSink, mode: PrintMode) -> Self {
        Self {
            sink,
            mode,
            lists: Vec::new(),
            atom: AtomState::None,
            skipped_lists: 0,
        }
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
            self.opaque_value(OpaqueKind::Object, Some("DepthLimit"))?;
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

    pub fn real_text(&mut self, text: &str) -> Result<(), PrintError> {
        if !valid_real_text(text) {
            return Err(PrintError::InvalidSequence);
        }
        self.scalar(text)
    }

    pub fn point(&mut self, coordinates: &[&str]) -> Result<(), PrintError> {
        if !matches!(coordinates.len(), 2 | 3)
            || coordinates
                .iter()
                .any(|coordinate| !valid_real_text(coordinate))
        {
            return Err(PrintError::InvalidSequence);
        }
        if self.skipped_lists != 0 {
            return self.poll_output();
        }
        self.require_no_atom()?;
        self.before_value()?;
        self.write("(")?;
        for (index, coordinate) in coordinates.iter().enumerate() {
            if index != 0 {
                self.write(" ")?;
            }
            self.write(coordinate)?;
        }
        self.write(")")
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
        if self.mode == PrintMode::Readable {
            self.write("\"")?;
        }
        self.atom = AtomState::String;
        Ok(())
    }

    pub fn string_chunk(&mut self, text: &str) -> Result<(), PrintError> {
        if text.len() > MAX_VALUE_TEXT_BYTES {
            return Err(PrintError::InvalidSequence);
        }
        if self.skipped_lists != 0 {
            return self.poll_output();
        }
        if self.atom != AtomState::String {
            return Err(PrintError::InvalidSequence);
        }
        if self.mode == PrintMode::Display || !text.chars().any(requires_readable_escape) {
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
        if self.mode == PrintMode::Readable {
            self.write("\"")?;
        }
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
        self.opaque_value(OpaqueKind::Entity, handle.as_deref())
    }

    pub fn selection_set(&mut self, number: Option<u64>) -> Result<(), PrintError> {
        let number = number.map(|number| number.to_string());
        self.opaque_value(OpaqueKind::SelectionSet, number.as_deref())
    }

    pub fn vla_object(&mut self, class_name: Option<&str>) -> Result<(), PrintError> {
        self.opaque_value(
            OpaqueKind::VlaObject,
            class_name.filter(|name| valid_label(name, MAX_CLASS_NAME_BYTES)),
        )
    }

    pub fn file(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::File, None)
    }

    pub fn function(&mut self, name: Option<&str>) -> Result<(), PrintError> {
        self.opaque_value(
            OpaqueKind::Function,
            name.filter(|name| valid_label(name, MAX_FUNCTION_NAME_BYTES)),
        )
    }

    pub fn error_object(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::Error, None)
    }

    pub fn unsupported(&mut self, native_type: Option<u32>) -> Result<(), PrintError> {
        let native_type = native_type.map(|native_type| format!("RT{native_type}"));
        self.opaque_value(OpaqueKind::Object, native_type.as_deref())
    }

    pub fn void(&mut self) -> Result<(), PrintError> {
        self.opaque_value(OpaqueKind::Void, None)
    }

    pub fn finish(self) -> Result<(), PrintError> {
        if self.atom != AtomState::None || !self.lists.is_empty() || self.skipped_lists != 0 {
            let _ = self.sink.flush();
            return Err(PrintError::InvalidSequence);
        }
        self.write("\n")?;
        match self.sink.flush() {
            EmitResult::Written => Ok(()),
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
            EmitResult::Written => Ok(()),
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
        if self.mode == PrintMode::Display || !text.contains('\\') {
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
            Self::Void => "Void",
        }
    }
}

fn valid_real_text(text: &str) -> bool {
    text.len() <= MAX_REAL_TEXT_BYTES
        && canonical_real_syntax(text)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
        && text.parse::<f64>().is_ok_and(f64::is_finite)
}

fn canonical_real_syntax(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == integer_start || bytes.get(index) != Some(&b'.') {
        return false;
    }
    index += 1;
    let fraction_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == fraction_start {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
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
mod tests {
    use super::*;
    use crate::execution::output::{OutputStream, channel};

    #[tokio::test]
    async fn concatenates_display_arguments_and_adds_one_newline() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Display);
        printer.begin_string().unwrap();
        printer.string_chunk("created: ").unwrap();
        printer.end_string().unwrap();
        printer.integer(12).unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(collect(stream).await, "created: 12\n");
    }

    #[tokio::test]
    async fn renders_readable_nested_strings_incrementally() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        printer.begin_list().unwrap();
        printer.integer(1).unwrap();
        printer.begin_string().unwrap();
        printer.string_chunk("a\"\\\u{1b}\u{2}\n\r\t中").unwrap();
        printer.end_string().unwrap();
        printer.nil().unwrap();
        printer.end_list().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(
            collect(stream).await,
            "(1 \"a\\\"\\\\\\e\\002\\n\\r\\t中\" nil)\n"
        );
    }

    #[tokio::test]
    async fn renders_proper_and_dotted_structure() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        printer.begin_list().unwrap();
        symbol(&mut printer, "A");
        printer.begin_list().unwrap();
        symbol(&mut printer, "B");
        printer.dot().unwrap();
        symbol(&mut printer, "C");
        printer.end_list().unwrap();
        printer.end_list().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(collect(stream).await, "(A (B . C))\n");
    }

    #[tokio::test]
    async fn applies_autolisp_backslash_semantics_to_symbols() {
        let (readable_sink, readable_stream) = channel();
        let readable_terminal = readable_sink.clone();
        let mut readable = ValuePrinter::new(readable_sink, PrintMode::Readable);
        symbol(&mut readable, "A\\B");
        readable.finish().unwrap();
        readable_terminal.finish();

        let (display_sink, display_stream) = channel();
        let display_terminal = display_sink.clone();
        let mut display = ValuePrinter::new(display_sink, PrintMode::Display);
        symbol(&mut display, "A\\B");
        display.finish().unwrap();
        display_terminal.finish();

        assert_eq!(collect(readable_stream).await, "A\\\\B\n");
        assert_eq!(collect(display_stream).await, "A\\B\n");
    }

    #[test]
    fn distinguishes_symbols_from_numeric_and_nil_tokens_across_chunks() {
        for chunks in [
            &["123"][..],
            &["+", "123"][..],
            &["-1", "23"][..],
            &["1", "e", "3"][..],
            &["1e", "-", "3"][..],
            &["n", "I", "l"][..],
        ] {
            let (sink, _stream) = channel();
            let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
            printer.begin_symbol().unwrap();
            for chunk in chunks {
                printer.symbol_chunk(chunk).unwrap();
            }
            assert_eq!(printer.end_symbol(), Err(PrintError::InvalidSequence));
        }

        for chunks in [
            &["+"][..],
            &["-"][..],
            &["123", "A"][..],
            &["1", "E"][..],
            &["1E", "+"][..],
        ] {
            let (sink, _stream) = channel();
            let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
            printer.begin_symbol().unwrap();
            for chunk in chunks {
                printer.symbol_chunk(chunk).unwrap();
            }
            printer.end_symbol().unwrap();
        }
    }

    #[tokio::test]
    async fn uses_kind_specific_opaque_displays() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Display);
        printer.begin_list().unwrap();
        printer.entity(Some("5a2")).unwrap();
        printer.selection_set(Some(7)).unwrap();
        printer.file().unwrap();
        printer.function(Some("TWICE")).unwrap();
        printer.function(Some("#<SUBR @123>")).unwrap();
        printer.end_list().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(
            collect(stream).await,
            "(#<Entity 5A2> #<SelectionSet 7> #<File> #<Function TWICE> #<Function>)\n"
        );
    }

    #[tokio::test]
    async fn accepts_only_autolisp_normalized_real_text() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        printer.begin_list().unwrap();
        for number in ["1.0", "0.0", "1.23457", "1.0e-12", "1.0e+20"] {
            printer.real_text(number).unwrap();
        }
        assert_eq!(printer.real_text("NaN"), Err(PrintError::InvalidSequence));
        assert_eq!(printer.real_text("inf"), Err(PrintError::InvalidSequence));
        assert_eq!(printer.real_text(".5"), Err(PrintError::InvalidSequence));
        assert_eq!(printer.real_text("1."), Err(PrintError::InvalidSequence));
        assert_eq!(printer.real_text("1.0e"), Err(PrintError::InvalidSequence));
        printer.end_list().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        assert_eq!(collect(stream).await, "(1.0 0.0 1.23457 1.0e-12 1.0e+20)\n");
    }

    #[test]
    fn rejects_malformed_or_oversized_events() {
        let (sink, _stream) = channel();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
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
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        for _ in 0..=MAX_VALUE_DEPTH {
            printer.begin_list().unwrap();
        }
        terminal.request_cancel();

        assert_eq!(
            printer.integer(1),
            Err(PrintError::Output(EmitResult::Cancelled))
        );
    }

    #[test]
    fn zero_output_atom_boundaries_observe_cancellation() {
        let (symbol_sink, _stream) = channel();
        let symbol_terminal = symbol_sink.clone();
        let mut symbol_printer = ValuePrinter::new(symbol_sink, PrintMode::Readable);
        symbol_printer.begin_symbol().unwrap();
        symbol_printer.symbol_chunk("A").unwrap();
        symbol_terminal.request_cancel();
        assert_eq!(
            symbol_printer.end_symbol(),
            Err(PrintError::Output(EmitResult::Cancelled))
        );

        let (string_sink, _stream) = channel();
        let string_terminal = string_sink.clone();
        let mut string_printer = ValuePrinter::new(string_sink, PrintMode::Display);
        string_printer.begin_string().unwrap();
        string_terminal.request_cancel();
        assert_eq!(
            string_printer.end_string(),
            Err(PrintError::Output(EmitResult::Cancelled))
        );
    }

    #[tokio::test]
    async fn escapes_dense_control_text_without_changing_bytes() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        printer.begin_string().unwrap();
        printer
            .string_chunk(&"\u{2}".repeat(MAX_VALUE_TEXT_BYTES))
            .unwrap();
        printer.end_string().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        let expected = format!("\"{}\"\n", "\\002".repeat(MAX_VALUE_TEXT_BYTES));
        assert_eq!(collect(stream).await, expected);
    }

    #[tokio::test]
    async fn replaces_excessive_depth_with_an_honest_opaque_value() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
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
        assert!(output.contains("#<Object DepthLimit>"));
        assert!(!output.contains('1'));
        assert_eq!(std::mem::size_of::<ListState>(), 1);
    }

    #[tokio::test]
    async fn renders_large_escaped_strings_without_a_composite_buffer() {
        let (sink, stream) = channel();
        let terminal = sink.clone();
        let mut printer = ValuePrinter::new(sink, PrintMode::Readable);
        printer.begin_string().unwrap();
        for _ in 0..10_000 {
            printer.string_chunk("\"中").unwrap();
        }
        printer.end_string().unwrap();
        printer.finish().unwrap();
        terminal.finish();

        let expected = format!("\"{}\"\n", "\\\"中".repeat(10_000));
        assert_eq!(collect(stream).await, expected);
    }

    fn symbol(printer: &mut ValuePrinter, text: &str) {
        printer.begin_symbol().unwrap();
        printer.symbol_chunk(text).unwrap();
        printer.end_symbol().unwrap();
    }

    async fn collect(mut stream: OutputStream) -> String {
        let mut output = String::new();
        while let Some(chunk) = stream.next_chunk().await {
            output.push_str(&chunk);
        }
        output
    }
}
