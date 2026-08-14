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

pub fn scan(source: &str) -> Result<Vec<FormSpan>, ScanError> {
    let mut cursor = Cursor::new(source);
    let mut forms = Vec::new();

    cursor.skip_trivia()?;
    while !cursor.is_end() {
        let byte_start = cursor.byte;
        let line = cursor.line;
        let column = cursor.column;
        cursor.scan_form()?;
        forms.push(FormSpan {
            byte_start,
            byte_end: cursor.byte,
            line,
            column,
        });
        cursor.skip_trivia()?;
    }

    Ok(forms)
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
        while self.peek().is_some_and(|character| {
            !character.is_whitespace() && !matches!(character, '(' | ')' | '"' | '\'' | ';')
        }) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_empty_and_comment_only_sources() {
        assert_eq!(scan("").unwrap(), []);
        assert_eq!(scan(" \t\r\n; line\r\n;| block\ncomment |; ").unwrap(), []);
    }

    #[test]
    fn scans_atoms_lists_and_adjacent_forms() {
        let source = "nil  42\n(foo (bar . baz))'qux\"text\"";
        let forms = scan(source).unwrap();
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
        let forms = scan(source).unwrap();

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
        let forms = scan(source).unwrap();
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
        let forms = scan(source).unwrap_err();

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

    fn assert_error(
        source: &str,
        expected: ScanErrorKind,
        expected_line: usize,
        expected_column: usize,
    ) {
        let error = scan(source).unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!((error.line, error.column), (expected_line, expected_column));
    }
}
