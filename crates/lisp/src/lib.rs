#![forbid(unsafe_code)]

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormSpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanErrorKind {
    UnexpectedCloseParen,
    UnterminatedList,
    UnterminatedString,
    UnterminatedBlockComment,
    MissingQuotedForm,
}

impl ScanErrorKind {
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnexpectedCloseParen => "unexpected closing parenthesis",
            Self::UnterminatedList => "unterminated list",
            Self::UnterminatedString => "unterminated string",
            Self::UnterminatedBlockComment => "unterminated block comment",
            Self::MissingQuotedForm => "quote is not followed by a form",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    pub kind: ScanErrorKind,
    pub line: usize,
    pub column: usize,
}

pub fn scan(source: &str) -> Scanner<'_> {
    Scanner::new(source)
}

pub fn validate(source: &str) -> Result<usize, ScanError> {
    scan(source).try_fold(0usize, |count, form| {
        form.map(|_| count.checked_add(1).expect("form count overflowed usize"))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanPosition {
    byte: usize,
    line: usize,
    column: usize,
    finished: bool,
}

pub struct Scanner<'a> {
    cursor: Cursor<'a>,
    finished: bool,
}

impl<'a> Scanner<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            cursor: Cursor::new(source),
            finished: false,
        }
    }

    pub const fn position(&self) -> ScanPosition {
        ScanPosition {
            byte: self.cursor.byte,
            line: self.cursor.line,
            column: self.cursor.column,
            finished: self.finished,
        }
    }

    pub const fn resume(source: &'a str, position: ScanPosition) -> Self {
        Self {
            cursor: Cursor {
                source,
                byte: position.byte,
                line: position.line,
                column: position.column,
            },
            finished: position.finished,
        }
    }
}

impl Iterator for Scanner<'_> {
    type Item = Result<FormSpan, ScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        if let Err(error) = self.cursor.skip_trivia() {
            self.finished = true;

            return Some(Err(error));
        }

        if self.cursor.is_end() {
            self.finished = true;

            return None;
        }

        let form = FormSpan {
            byte_start: self.cursor.byte,
            byte_end: 0,
            line: self.cursor.line,
            column: self.cursor.column,
        };

        match self.cursor.scan_form() {
            Ok(()) => Some(Ok(FormSpan {
                byte_end: self.cursor.byte,
                ..form
            })),
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

struct Cursor<'a> {
    source: &'a str,
    byte: usize,
    line: usize,
    column: usize,
}

impl<'a> Cursor<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            byte: 0,
            line: 1,
            column: 1,
        }
    }

    fn scan_form(&mut self) -> Result<(), ScanError> {
        let mut quote = None;

        while self.peek() == Some('\'') {
            quote.get_or_insert((self.line, self.column));
            self.advance();
            self.skip_trivia()?;
        }

        if self.is_end() {
            let (line, column) = quote.expect("only quoted forms reach this branch");

            return Err(self.error_at(ScanErrorKind::MissingQuotedForm, line, column));
        }

        match self.peek() {
            Some('(') => self.scan_list(),
            Some(')') => Err(self.error(ScanErrorKind::UnexpectedCloseParen)),
            Some('"') => self.scan_string(),
            Some(_) => {
                self.scan_atom();
                Ok(())
            }
            None => unreachable!(),
        }
    }

    fn scan_list(&mut self) -> Result<(), ScanError> {
        let start = (self.line, self.column);
        let mut depth = 0usize;

        loop {
            match self.peek() {
                Some('(') => {
                    depth += 1;
                    self.advance();
                }
                Some(')') => {
                    depth -= 1;
                    self.advance();

                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some('"') => self.scan_string()?,
                Some(';') => self.skip_comment()?,
                Some(_) => self.advance(),
                None => {
                    return Err(self.error_at(ScanErrorKind::UnterminatedList, start.0, start.1));
                }
            }
        }
    }

    fn scan_string(&mut self) -> Result<(), ScanError> {
        let start = (self.line, self.column);
        self.advance();

        loop {
            match self.peek() {
                Some('"') => {
                    self.advance();

                    return Ok(());
                }
                Some('\\') => {
                    self.advance();

                    if self.is_end() {
                        return Err(self.error_at(
                            ScanErrorKind::UnterminatedString,
                            start.0,
                            start.1,
                        ));
                    }

                    self.advance();
                }
                Some(_) => self.advance(),
                None => {
                    return Err(self.error_at(ScanErrorKind::UnterminatedString, start.0, start.1));
                }
            }
        }
    }

    fn scan_atom(&mut self) {
        let token_end = self
            .remaining()
            .find(is_atom_delimiter)
            .unwrap_or(self.source.len() - self.byte);
        let token = &self.remaining()[..token_end];
        let byte_count = match token.find('.') {
            Some(0) => token.len(),
            Some(period) if !is_decimal(token) => period,
            _ => token.len(),
        };

        let end = self.byte + byte_count;

        while self.byte < end {
            self.advance();
        }
    }

    fn skip_trivia(&mut self) -> Result<(), ScanError> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.advance();
            }

            if self.peek() != Some(';') {
                return Ok(());
            }

            self.skip_comment()?;
        }
    }

    fn skip_comment(&mut self) -> Result<(), ScanError> {
        if self.remaining().starts_with(";|") {
            let start = (self.line, self.column);
            self.advance();
            self.advance();

            while !self.remaining().starts_with("|;") {
                if self.is_end() {
                    return Err(self.error_at(
                        ScanErrorKind::UnterminatedBlockComment,
                        start.0,
                        start.1,
                    ));
                }

                self.advance();
            }

            self.advance();
            self.advance();
        } else {
            while self
                .peek()
                .is_some_and(|character| !matches!(character, '\r' | '\n'))
            {
                self.advance();
            }
        }

        Ok(())
    }

    fn advance(&mut self) {
        match self.peek() {
            Some('\r') => {
                self.byte += 1;

                if self.peek() == Some('\n') {
                    self.byte += 1;
                }

                self.line += 1;
                self.column = 1;
            }
            Some('\n') => {
                self.byte += 1;
                self.line += 1;
                self.column = 1;
            }
            Some(character) => {
                self.byte += character.len_utf8();
                self.column += 1;
            }
            None => {}
        }
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.byte..]
    }

    fn is_end(&self) -> bool {
        self.byte == self.source.len()
    }

    fn error(&self, kind: ScanErrorKind) -> ScanError {
        self.error_at(kind, self.line, self.column)
    }

    const fn error_at(&self, kind: ScanErrorKind, line: usize, column: usize) -> ScanError {
        ScanError { kind, line, column }
    }
}

fn is_atom_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '(' | ')' | '"' | '\'' | ';')
}

fn is_decimal(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut cursor = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let integer_start = cursor;

    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }

    if cursor == integer_start || bytes.get(cursor) != Some(&b'.') {
        return false;
    }

    cursor += 1;

    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }

    if matches!(bytes.get(cursor), Some(b'e') | Some(b'E')) {
        cursor += 1;

        if matches!(bytes.get(cursor), Some(b'+') | Some(b'-')) {
            cursor += 1;
        }

        let exponent_start = cursor;

        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }

        if cursor == exponent_start {
            return false;
        }
    }

    cursor == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_empty_and_comment_only_sources() {
        assert_eq!(forms(""), []);
        assert_eq!(forms(" \t\r\n; line\r\n;| block\ncomment |; "), []);
    }

    #[test]
    fn scans_atoms_lists_and_adjacent_forms() {
        let source = "nil  42\n(foo (bar . baz))'qux\"text\"";
        let forms = forms(source);
        let text = forms
            .iter()
            .map(|span| &source[span.byte_start..span.byte_end])
            .collect::<Vec<_>>();

        assert_eq!(text, ["nil", "42", "(foo (bar . baz))", "'qux", "\"text\""]);
        assert_eq!((forms[2].line, forms[2].column), (2, 1));
    }

    #[test]
    fn keeps_quote_prefixes_and_intervening_trivia_with_their_form() {
        let source = "'' ; explain\n ;| more |; (a b) next";
        let forms = forms(source);

        assert_eq!(forms.len(), 2);
        assert_eq!(
            &source[forms[0].byte_start..forms[0].byte_end],
            "'' ; explain\n ;| more |; (a b)"
        );
        assert_eq!(&source[forms[1].byte_start..forms[1].byte_end], "next");
    }

    #[test]
    fn ignores_reader_characters_inside_strings_and_comments() {
        let source = "(list \"\\\" ); (still string)\" ; line )\n ;| ) ( |; '(x))\nend";
        let forms = forms(source);
        let text = forms
            .iter()
            .map(|span| &source[span.byte_start..span.byte_end])
            .collect::<Vec<_>>();

        assert_eq!(
            text,
            [
                "(list \"\\\" ); (still string)\" ; line )\n ;| ) ( |; '(x))",
                "end"
            ]
        );
    }

    #[test]
    fn reports_unicode_locations_across_crlf() {
        let source = "字\r\n  (未完";
        let forms = scan(source).find_map(Result::err).unwrap();

        assert_eq!(forms.kind, ScanErrorKind::UnterminatedList);
        assert_eq!((forms.line, forms.column), (2, 3));
    }

    #[test]
    fn reports_each_incomplete_or_invalid_shape() {
        assert_error(")", ScanErrorKind::UnexpectedCloseParen, 1, 1);
        assert_error("(a\n b", ScanErrorKind::UnterminatedList, 1, 1);
        assert_error("x \"bad", ScanErrorKind::UnterminatedString, 1, 3);
        assert_error("x\n;| bad", ScanErrorKind::UnterminatedBlockComment, 2, 1);
        assert_error(" \n' ; none", ScanErrorKind::MissingQuotedForm, 2, 1);
    }

    #[test]
    fn follows_autolisp_period_and_decimal_boundaries() {
        let source = "a.b .b 1.2 .5 1. 1.a a.1 +.5 1e-3 1.2.3";
        let spans = forms(source);
        let text = spans
            .iter()
            .map(|span| &source[span.byte_start..span.byte_end])
            .collect::<Vec<_>>();

        assert_eq!(
            text,
            [
                "a", ".b", ".b", "1.2", ".5", "1.", "1", ".a", "a", ".1", "+", ".5", "1e-3", "1",
                ".2.3"
            ]
        );
    }

    #[test]
    fn resumes_from_a_constant_size_position() {
        let source = "first (second) third";
        let mut scanner = scan(source);
        assert_eq!(scanner.next().unwrap().unwrap().byte_end, 5);
        let position = scanner.position();
        let mut resumed = Scanner::resume(source, position);

        let second = resumed.next().unwrap().unwrap();
        assert_eq!(&source[second.byte_start..second.byte_end], "(second)");
        assert_eq!(validate(source).unwrap(), 3);
    }

    fn assert_error(
        source: &str,
        expected: ScanErrorKind,
        expected_line: usize,
        expected_column: usize,
    ) {
        let error = scan(source)
            .find_map(Result::err)
            .expect("source should fail scanning");
        assert_eq!(error.kind, expected);
        assert_eq!((error.line, error.column), (expected_line, expected_column));
    }

    fn forms(source: &str) -> Vec<FormSpan> {
        scan(source).collect::<Result<_, _>>().unwrap()
    }
}
