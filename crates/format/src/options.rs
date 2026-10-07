/// How a level of indentation is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Spaces(usize),
    Tab,
}

impl Indent {
    pub fn unit(self) -> String {
        match self {
            Indent::Spaces(width) => " ".repeat(width.max(1)),
            Indent::Tab => "\t".to_string(),
        }
    }
}

/// The case keywords are written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeywordCase {
    Upper,
    Lower,
    /// As they were written.
    Preserve,
}

/// What the formatter may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent: Indent,
    pub keyword_case: KeywordCase,
    /// A comma that separates the items of a list laid out a line each starts the next line,
    /// rather than ending the line before.
    pub leading_commas: bool,
}

impl Default for FormatOptions {
    fn default() -> FormatOptions {
        FormatOptions {
            indent: Indent::Spaces(4),
            keyword_case: KeywordCase::Upper,
            leading_commas: false,
        }
    }
}
