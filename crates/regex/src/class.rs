//! Character sets. Unicode tables and simple case folding come from `regex-syntax`; this module
//! builds `ClassUnicode`s and freezes them into sorted range lists for matching.
//!
//! `\d`, `\w`, `\s` and `\b` are Unicode-aware (as in `regex-automata`, and PCRE2 with UCP);
//! POSIX classes such as `[:alpha:]` are ASCII (as in both).

use regex_syntax::hir::{Class, ClassUnicode, ClassUnicodeRange, HirKind};

/// A frozen set of chars: sorted, non-overlapping inclusive ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CharSet(Box<[(char, char)]>);

impl CharSet {
    pub fn new(class: &ClassUnicode) -> Self {
        Self(class.ranges().iter().map(|r| (r.start(), r.end())).collect())
    }

    pub fn contains(&self, c: char) -> bool {
        let i = self.0.partition_point(|&(_, hi)| hi < c);
        self.0.get(i).is_some_and(|&(lo, _)| lo <= c)
    }

    pub fn class(&self) -> ClassUnicode {
        ClassUnicode::new(self.0.iter().map(|&(lo, hi)| ClassUnicodeRange::new(lo, hi)))
    }

    /// The only char in the set, if it has exactly one.
    pub fn single(&self) -> Option<char> {
        match *self.0 {
            [(lo, hi)] if lo == hi => Some(lo),
            _ => None,
        }
    }
}

pub(crate) fn of_char(c: char) -> ClassUnicode {
    ClassUnicode::new([ClassUnicodeRange::new(c, c)])
}

pub(crate) fn of_ranges(ranges: &[(char, char)]) -> ClassUnicode {
    ClassUnicode::new(ranges.iter().map(|&(lo, hi)| ClassUnicodeRange::new(lo, hi)))
}

/// Adds the simple case folds of every char in the class (`ß` stays apart from `ss`).
pub(crate) fn fold(class: &mut ClassUnicode) {
    // ponytail: the "unicode-case" feature is on by default, so this never fails.
    let _ = class.try_case_fold_simple();
}

/// Whether `a` and `b` are equal under simple case folding.
pub(crate) fn fold_eq(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    let mut c = of_char(a);
    fold(&mut c);
    c.ranges().iter().any(|r| r.start() <= b && b <= r.end())
}

pub(crate) fn is_word(c: char) -> bool {
    regex_syntax::try_is_word_character(c).unwrap_or(false)
}

/// Parses a single class with `regex-syntax`; used for tables we don't keep ourselves.
fn via_regex_syntax(pattern: &str) -> Option<ClassUnicode> {
    let hir = regex_syntax::ParserBuilder::new().build().parse(pattern).ok()?;
    match hir.kind() {
        HirKind::Class(Class::Unicode(c)) => Some(c.clone()),
        HirKind::Literal(lit) => {
            let s = std::str::from_utf8(&lit.0).ok()?;
            let mut chars = s.chars();
            let c = chars.next()?;
            chars.next().is_none().then(|| of_char(c))
        }
        _ => None,
    }
}

/// `\p{name}` (or `\pL`); `None` if the property is unknown.
pub(crate) fn property(name: &str) -> Option<ClassUnicode> {
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | '=' | ':' | '.' | '&'));
    if !ok {
        return None;
    }
    via_regex_syntax(&format!("\\p{{{name}}}"))
}

/// `[:name:]`; `None` if the name is unknown.
pub(crate) fn posix(name: &str) -> Option<ClassUnicode> {
    const NAMES: [&str; 14] =
        ["alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space", "upper", "word", "xdigit"];
    if !NAMES.contains(&name) {
        return None;
    }
    via_regex_syntax(&format!("[[:{name}:]]"))
}

/// The Perl-style class for `\d \w \s \h \v` (lower case letter only).
pub(crate) fn perl(letter: char) -> Option<ClassUnicode> {
    match letter {
        'd' => via_regex_syntax("\\d"),
        'w' => via_regex_syntax("\\w"),
        's' => via_regex_syntax("\\s"),
        'h' => Some(of_ranges(&[
            ('\t', '\t'),
            (' ', ' '),
            ('\u{A0}', '\u{A0}'),
            ('\u{1680}', '\u{1680}'),
            ('\u{180E}', '\u{180E}'),
            ('\u{2000}', '\u{200A}'),
            ('\u{202F}', '\u{202F}'),
            ('\u{205F}', '\u{205F}'),
            ('\u{3000}', '\u{3000}'),
        ])),
        'v' => Some(of_ranges(&[('\n', '\r'), ('\u{85}', '\u{85}'), ('\u{2028}', '\u{2029}')])),
        _ => None,
    }
}

/// Vertical space chars that `\R` matches on their own.
pub(crate) fn is_vertical(c: char) -> bool {
    matches!(c, '\n'..='\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        let w = CharSet::new(&perl('w').unwrap());
        assert!(w.contains('é') && w.contains('_') && !w.contains(' '));
        let greek = CharSet::new(&property("Greek").unwrap());
        assert!(greek.contains('λ') && !greek.contains('l'));
        assert!(property("Nope").is_none() && property("L}|x").is_none());
        let alpha = CharSet::new(&posix("alpha").unwrap());
        assert!(alpha.contains('a') && !alpha.contains('é'));
        assert!(fold_eq('k', 'K') && fold_eq('k', '\u{212A}') && !fold_eq('ß', 's'));
    }
}
