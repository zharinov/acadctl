use std::collections::VecDeque;

use unicode_width::UnicodeWidthStr;

use super::PrintError;
use crate::exec::output::{EmitResult, OutputSink};

const PRINT_WIDTH: usize = 100;
const INDENT_WIDTH: usize = 2;
const SPACE_CHUNK: &str = "                                                                ";

pub(super) struct Layout {
    sink: OutputSink,
    pending: VecDeque<Token>,
    breakable_groups: Vec<GroupMode>,
    forced_flat_depth: usize,
    column: usize,
}

enum Token {
    Begin(GroupKind),
    End,
    Text(String),
    Line { flat: &'static str, indent: usize },
}

#[derive(Clone, Copy)]
enum GroupKind {
    Breakable,
    ForcedFlat,
}

#[derive(Clone, Copy)]
enum GroupMode {
    Flat,
    Broken,
}

enum Fit {
    Yes,
    No,
    Unknown,
}

impl Layout {
    pub(super) fn new(sink: OutputSink) -> Self {
        Self {
            sink,
            pending: VecDeque::new(),
            breakable_groups: Vec::new(),
            forced_flat_depth: 0,
            column: 0,
        }
    }

    pub(super) fn begin_group(&mut self, depth: usize) -> Result<(), PrintError> {
        let kind = if indent_for_depth(depth) < PRINT_WIDTH {
            GroupKind::Breakable
        } else {
            GroupKind::ForcedFlat
        };
        self.push(Token::Begin(kind))
    }

    pub(super) fn end_group(&mut self) -> Result<(), PrintError> {
        self.push(Token::End)
    }

    pub(super) fn text(&mut self, text: &str) -> Result<(), PrintError> {
        self.poll()?;

        if text.is_empty() {
            return Ok(());
        }
        if self.pending.is_empty() {
            return self.emit_text(text);
        }

        let mut owned = String::new();
        owned
            .try_reserve(text.len())
            .map_err(|_| PrintError::LimitExceeded)?;
        owned.push_str(text);
        self.push_polled(Token::Text(owned))
    }

    pub(super) fn line(&mut self, flat: &'static str, depth: usize) -> Result<(), PrintError> {
        self.push(Token::Line {
            flat,
            indent: indent_for_depth(depth),
        })
    }

    pub(super) fn poll(&self) -> Result<(), PrintError> {
        match self.sink.emit("") {
            EmitResult::Continue => Ok(()),
            result => Err(PrintError::Output(result)),
        }
    }

    pub(super) fn finish(mut self) -> Result<(), PrintError> {
        self.drain()?;
        if !self.pending.is_empty()
            || !self.breakable_groups.is_empty()
            || self.forced_flat_depth != 0
        {
            let _ = self.sink.flush();

            return Err(PrintError::InvalidSequence);
        }

        self.emit_text("\n")?;
        match self.sink.flush() {
            EmitResult::Continue => Ok(()),
            result => Err(PrintError::Output(result)),
        }
    }

    fn push(&mut self, token: Token) -> Result<(), PrintError> {
        self.poll()?;
        self.push_polled(token)
    }

    fn push_polled(&mut self, token: Token) -> Result<(), PrintError> {
        self.pending
            .try_reserve(1)
            .map_err(|_| PrintError::LimitExceeded)?;
        self.pending.push_back(token);
        self.drain()
    }

    fn drain(&mut self) -> Result<(), PrintError> {
        loop {
            let Some(token) = self.pending.front() else {
                return Ok(());
            };

            match token {
                Token::Begin(GroupKind::Breakable) => {
                    let remaining = PRINT_WIDTH.saturating_sub(self.column);
                    let mode = match self.group_fits(remaining) {
                        Fit::Yes => GroupMode::Flat,
                        Fit::No => GroupMode::Broken,
                        Fit::Unknown => return Ok(()),
                    };
                    self.pending.pop_front();
                    self.breakable_groups
                        .try_reserve(1)
                        .map_err(|_| PrintError::LimitExceeded)?;
                    self.breakable_groups.push(mode);
                }
                Token::Begin(GroupKind::ForcedFlat) => {
                    self.pending.pop_front();
                    self.forced_flat_depth = self
                        .forced_flat_depth
                        .checked_add(1)
                        .ok_or(PrintError::LimitExceeded)?;
                }
                Token::End => {
                    if self.forced_flat_depth != 0 {
                        self.forced_flat_depth -= 1;
                    } else if self.breakable_groups.pop().is_none() {
                        return Err(PrintError::InvalidSequence);
                    }
                    self.pending.pop_front();
                }
                Token::Text(_) => {
                    let Some(Token::Text(text)) = self.pending.pop_front() else {
                        unreachable!("front token is text")
                    };
                    self.emit_text(&text)?;
                }
                Token::Line { .. } => {
                    let Some(Token::Line { flat, indent }) = self.pending.pop_front() else {
                        unreachable!("front token is a line")
                    };
                    if self.forced_flat_depth != 0 {
                        self.emit_text(flat)?;
                    } else {
                        match self.breakable_groups.last() {
                            Some(GroupMode::Flat) => self.emit_text(flat)?,
                            Some(GroupMode::Broken) => self.emit_newline(indent)?,
                            None => return Err(PrintError::InvalidSequence),
                        }
                    }
                }
            }
        }
    }

    fn group_fits(&self, remaining: usize) -> Fit {
        let mut depth = 0usize;
        let mut width = 0usize;

        for token in &self.pending {
            match token {
                Token::Begin(_) => depth += 1,
                Token::End => {
                    depth -= 1;
                    if depth == 0 {
                        return Fit::Yes;
                    }
                }
                Token::Text(text) => width = width.saturating_add(display_width(text)),
                Token::Line { flat, .. } => width = width.saturating_add(display_width(flat)),
            }

            if width > remaining {
                return Fit::No;
            }
        }

        Fit::Unknown
    }

    fn emit_text(&mut self, text: &str) -> Result<(), PrintError> {
        match self.sink.emit(text) {
            EmitResult::Continue => {
                self.column = self.column.saturating_add(display_width(text));
                Ok(())
            }
            result => Err(PrintError::Output(result)),
        }
    }

    fn emit_newline(&mut self, indent: usize) -> Result<(), PrintError> {
        match self.sink.emit("\n") {
            EmitResult::Continue => {}
            result => return Err(PrintError::Output(result)),
        }

        let mut remaining = indent;
        while remaining != 0 {
            let count = remaining.min(SPACE_CHUNK.len());
            match self.sink.emit(&SPACE_CHUNK[..count]) {
                EmitResult::Continue => remaining -= count,
                result => return Err(PrintError::Output(result)),
            }
        }

        self.column = indent;
        Ok(())
    }
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn indent_for_depth(depth: usize) -> usize {
    depth.saturating_mul(INDENT_WIDTH)
}
