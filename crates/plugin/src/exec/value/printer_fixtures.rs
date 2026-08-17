use super::*;
use crate::exec::output::{OutputStream, channel};

#[derive(Clone, Copy, Debug)]
enum Chunking {
    Whole,
    Characters,
}

#[derive(Debug)]
enum FixtureValue {
    Nil,
    True,
    Integer(i64),
    Real(f64),
    String(String),
    Symbol(String),
    List {
        values: Vec<Self>,
        tail: Option<Box<Self>>,
    },
    Entity(Option<String>),
    Opaque(FixtureOpaque),
}

#[derive(Debug)]
enum FixtureOpaque {
    SelectionSet,
    VlaObject,
    File,
    Function,
    Error,
    Object(Option<String>),
    Cycle,
    TooDeep,
}

async fn run_fixture(name: &str, expected: &str) {
    let value = Parser::parse(expected)
        .unwrap_or_else(|error| panic!("printer fixture `{name}` does not parse: {error}"));

    for chunking in [Chunking::Whole, Chunking::Characters] {
        let actual = format_value(&value, chunking)
            .await
            .unwrap_or_else(|error| {
                panic!("printer fixture `{name}` failed in {chunking:?} mode: {error:?}")
            });

        if actual == expected {
            continue;
        }

        let mismatch = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(actual, expected)| actual != expected)
            .unwrap_or_else(|| actual.len().min(expected.len()));
        panic!(
            "printer fixture `{name}` differs in {chunking:?} mode at byte {mismatch}\nexpected: {expected:?}\n  actual: {actual:?}"
        );
    }
}

#[tokio::test]
async fn fixtures() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/printer");
    let mut paths = std::fs::read_dir(directory)
        .expect("read printer fixtures")
        .map(|entry| entry.expect("read printer fixture").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("printer fixture name is UTF-8");
        let expected = std::fs::read_to_string(&path).expect("read printer fixture");
        run_fixture(name, &expected).await;
    }
}

async fn format_value(value: &FixtureValue, chunking: Chunking) -> Result<String, PrintError> {
    let (sink, stream) = channel();
    let terminal = sink.clone();
    let mut printer = ValuePrinter::new(sink);
    replay(value, &mut printer, chunking)?;

    if printer.root_values() != 1 {
        return Err(PrintError::InvalidSequence);
    }

    printer.finish()?;
    terminal.finish();
    Ok(collect(stream).await)
}

fn replay(
    value: &FixtureValue,
    printer: &mut ValuePrinter,
    chunking: Chunking,
) -> Result<(), PrintError> {
    match value {
        FixtureValue::Nil => printer.nil(),
        FixtureValue::True => printer.true_value(),
        FixtureValue::Integer(value) => printer.integer(*value),
        FixtureValue::Real(value) => printer.real(*value),
        FixtureValue::String(text) => {
            printer.begin_string()?;
            write_chunks(text, chunking, |chunk| printer.string_chunk(chunk))?;
            printer.end_string()
        }
        FixtureValue::Symbol(text) => {
            printer.begin_symbol()?;
            write_chunks(text, chunking, |chunk| printer.symbol_chunk(chunk))?;
            printer.end_symbol()
        }
        FixtureValue::List { values, tail } => {
            printer.begin_list()?;

            for value in values {
                replay(value, printer, chunking)?;
            }

            if let Some(tail) = tail {
                printer.dot()?;
                replay(tail, printer, chunking)?;
            }

            printer.end_list()
        }
        FixtureValue::Entity(handle) => printer.entity(handle.as_deref()),
        FixtureValue::Opaque(kind) => match kind {
            FixtureOpaque::SelectionSet => printer.selection_set(),
            FixtureOpaque::VlaObject => printer.vla_object(),
            FixtureOpaque::File => printer.file(),
            FixtureOpaque::Function => printer.function(),
            FixtureOpaque::Error => printer.error_object(),
            FixtureOpaque::Object(label) => printer.object(label.as_deref()),
            FixtureOpaque::Cycle => printer.cycle(),
            FixtureOpaque::TooDeep => printer.too_deep(),
        },
    }
}

fn write_chunks(
    text: &str,
    chunking: Chunking,
    mut write: impl FnMut(&str) -> Result<(), PrintError>,
) -> Result<(), PrintError> {
    match chunking {
        Chunking::Whole => write(text),
        Chunking::Characters => {
            write("")?;

            for (start, character) in text.char_indices() {
                write(&text[start..start + character.len_utf8()])?;
            }

            write("")
        }
    }
}

async fn collect(stream: OutputStream) -> String {
    let mut output = String::new();

    while let Some(chunk) = stream.next_chunk().await {
        output.push_str(&chunk);
    }

    output
}

struct Parser<'a> {
    source: &'a str,
    byte: usize,
}

impl<'a> Parser<'a> {
    fn parse(source: &'a str) -> Result<FixtureValue, String> {
        let mut parser = Self { source, byte: 0 };
        parser.skip_whitespace();
        let value = parser.value()?;
        parser.skip_whitespace();

        if parser.byte != source.len() {
            return Err(parser.error("expected exactly one value"));
        }

        Ok(value)
    }

    fn value(&mut self) -> Result<FixtureValue, String> {
        match self.peek() {
            Some('(') => self.list(),
            Some('"') => self.string().map(FixtureValue::String),
            Some('#') if self.remaining().starts_with("#<") => self.opaque(),
            Some(_) => self.atom(),
            None => Err(self.error("expected a value")),
        }
    }

    fn list(&mut self) -> Result<FixtureValue, String> {
        self.expect('(')?;
        self.skip_whitespace();
        let mut values = Vec::new();
        let mut tail = None;

        while self.peek() != Some(')') {
            if self.peek() == Some('.') && self.followed_by_delimiter(1) {
                if values.is_empty() {
                    return Err(self.error("a dotted list needs a head value"));
                }

                self.bump();
                self.skip_whitespace();
                tail = Some(Box::new(self.value()?));
                self.skip_whitespace();

                if self.peek() != Some(')') {
                    return Err(self.error("a dotted list must end after its tail"));
                }

                break;
            }

            values.push(self.value()?);
            self.skip_whitespace();

            if self.peek().is_none() {
                return Err(self.error("unterminated list"));
            }
        }

        self.expect(')')?;
        Ok(Self::recognize_list(values, tail))
    }

    fn recognize_list(
        mut values: Vec<FixtureValue>,
        tail: Option<Box<FixtureValue>>,
    ) -> FixtureValue {
        if tail.is_none()
            && values.len() == 2
            && matches!(&values[0], FixtureValue::Symbol(name) if name.eq_ignore_ascii_case("handent"))
            && matches!(&values[1], FixtureValue::String(handle) if valid_handle(handle))
        {
            let FixtureValue::String(handle) = values.pop().expect("entity handle exists") else {
                unreachable!()
            };
            return FixtureValue::Entity(Some(handle));
        }

        FixtureValue::List { values, tail }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut output = String::new();

        loop {
            let Some(character) = self.bump() else {
                return Err(self.error("unterminated string"));
            };

            match character {
                '"' => return Ok(output),
                '\\' => output.push(self.escape()?),
                character if character.is_control() => {
                    return Err(self.error("literal control character in string"));
                }
                character => output.push(character),
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let Some(character) = self.bump() else {
            return Err(self.error("unterminated string escape"));
        };

        match character {
            '"' => Ok('"'),
            '\\' => Ok('\\'),
            'e' => Ok('\u{1b}'),
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            '0'..='7' => {
                let second = self.octal_digit()?;
                let third = self.octal_digit()?;
                let value =
                    character.to_digit(8).expect("matched octal digit") * 64 + second * 8 + third;
                char::from_u32(value).ok_or_else(|| self.error("invalid octal string escape"))
            }
            _ => Err(self.error("unsupported string escape")),
        }
    }

    fn octal_digit(&mut self) -> Result<u32, String> {
        self.bump()
            .and_then(|character| character.to_digit(8))
            .ok_or_else(|| self.error("octal string escape needs three digits"))
    }

    fn opaque(&mut self) -> Result<FixtureValue, String> {
        self.byte += 2;
        let start = self.byte;

        while !matches!(self.peek(), Some('>') | None) {
            self.bump();
        }

        let content = &self.source[start..self.byte];
        self.expect('>')?;
        let (kind, label) = content
            .split_once(' ')
            .map_or((content, None), |(kind, label)| (kind, Some(label)));
        let opaque = match (kind, label) {
            ("Entity", None) => return Ok(FixtureValue::Entity(None)),
            ("SelectionSet", None) => FixtureOpaque::SelectionSet,
            ("VlaObject", None) => FixtureOpaque::VlaObject,
            ("File", None) => FixtureOpaque::File,
            ("Function", None) => FixtureOpaque::Function,
            ("Error", None) => FixtureOpaque::Error,
            ("Object", None) => FixtureOpaque::Object(None),
            ("Object", Some(label)) if fixture_label(label) => {
                FixtureOpaque::Object(Some(label.to_owned()))
            }
            ("Cycle", None) => FixtureOpaque::Cycle,
            ("TooDeep", None) => FixtureOpaque::TooDeep,
            _ => return Err(self.error("unknown or malformed opaque value")),
        };
        Ok(FixtureValue::Opaque(opaque))
    }

    fn atom(&mut self) -> Result<FixtureValue, String> {
        let start = self.byte;

        while self
            .peek()
            .is_some_and(|character| !character.is_whitespace() && !matches!(character, '(' | ')'))
        {
            self.bump();
        }

        let token = &self.source[start..self.byte];
        if token.is_empty() {
            return Err(self.error("expected an atom"));
        }
        if token.eq_ignore_ascii_case("nil") {
            return Ok(FixtureValue::Nil);
        }
        if token.eq_ignore_ascii_case("t") {
            return Ok(FixtureValue::True);
        }
        if integer_token(token) {
            return token
                .parse()
                .map(FixtureValue::Integer)
                .map_err(|_| self.error("integer is outside the supported range"));
        }
        if real_token(token) {
            return token
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(FixtureValue::Real)
                .ok_or_else(|| self.error("real is outside the supported range"));
        }

        let symbol = decode_symbol(token).map_err(|message| self.error(message))?;
        Ok(FixtureValue::Symbol(symbol))
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        match self.bump() {
            Some(actual) if actual == expected => Ok(()),
            _ => Err(self.error(&format!("expected `{expected}`"))),
        }
    }

    fn followed_by_delimiter(&self, bytes: usize) -> bool {
        self.remaining()[bytes..]
            .chars()
            .next()
            .is_none_or(|character| character.is_whitespace() || character == ')')
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.byte..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.byte += character.len_utf8();
        Some(character)
    }

    fn error(&self, message: &str) -> String {
        format!("{message} at byte {}", self.byte)
    }
}

fn valid_handle(handle: &str) -> bool {
    !handle.is_empty() && handle.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn fixture_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '<' | '>' | '@' | '"' | '\\')
        })
}

fn integer_token(token: &str) -> bool {
    let digits = unsigned(token).as_bytes();
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

fn real_token(token: &str) -> bool {
    let token = unsigned(token);
    let Some((integer, fraction_and_exponent)) = token.split_once('.') else {
        return false;
    };
    if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }

    let (fraction, exponent) =
        fraction_and_exponent
            .find(['e', 'E'])
            .map_or((fraction_and_exponent, None), |index| {
                (
                    &fraction_and_exponent[..index],
                    Some(&fraction_and_exponent[index + 1..]),
                )
            });
    if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }

    exponent.is_none_or(|exponent| {
        let digits = unsigned(exponent).as_bytes();
        !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
    })
}

fn unsigned(token: &str) -> &str {
    token
        .strip_prefix('+')
        .or_else(|| token.strip_prefix('-'))
        .unwrap_or(token)
}

fn decode_symbol(token: &str) -> Result<String, &'static str> {
    let mut characters = token.chars();
    let mut symbol = String::new();

    while let Some(character) = characters.next() {
        if character != '\\' {
            symbol.push(character);
            continue;
        }

        if characters.next() != Some('\\') {
            return Err("symbol backslashes must be doubled");
        }
        symbol.push('\\');
    }

    if symbol.is_empty()
        || !symbol.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '(' | ')' | '"' | ';' | '\'' | '.')
        })
    {
        return Err("invalid symbol");
    }
    Ok(symbol)
}
