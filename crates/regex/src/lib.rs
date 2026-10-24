#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

//! A PCRE2-style backtracking regex engine in pure Rust.
//!
//! It supports what a linear-time engine can't: backreferences, lookaround, recursion and
//! subroutine calls, conditionals, atomic groups and possessive quantifiers, and `\K`. The
//! syntax is listed in the README.
//!
//! A backtracker can take exponential time, so every search spends a caller-owned [`Budget`],
//! and the depth, stack, pattern and program sizes are capped ([`Options`]). Running out of any
//! of them ends in [`Error::TooComplex`]. Nothing recurses on the Rust stack: the parser,
//! compiler and matcher all keep their own heap stacks.
//!
//! Matching is leftmost-first (Perl), one `char` at a time. Positions are byte offsets.
//!
//! ```
//! use craft_regex::{Budget, Options, Regex};
//!
//! let re = Regex::new(r"\b(\w+) \1\b", &Options::default())?;
//! let hay = "this is is a test";
//! let mut budget = Budget::new(10_000_000);
//! let caps = re.captures_at(hay, 0..hay.len(), &mut budget)?.expect("a doubled word");
//! assert_eq!(caps.get(0), Some(5..10));
//! assert_eq!(caps.get(1), Some(5..7));
//! # Ok::<(), craft_regex::Error>(())
//! ```

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

mod ast;
mod class;
mod compile;
mod parse;
mod vm;

/// How a pattern is compiled and how far it may grow.
#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    /// `(?i)` for the whole pattern.
    pub case_insensitive: bool,
    /// `(?m)`: `^` and `$` also match at line ends.
    pub multiline: bool,
    /// `(?s)`: `.` also matches the newline characters.
    pub dot_all: bool,
    /// The characters that end a line, for `^`, `$`, `.` and `\N`.
    pub newlines: &'a [char],
    /// Maximum group nesting while parsing and recursion depth while matching.
    pub max_depth: u32,
    /// Maximum backtrack stack entries while matching.
    pub max_stack: usize,
    /// Maximum pattern length in chars.
    pub max_pattern: usize,
    /// Maximum compiled program size in instructions.
    pub max_insts: usize,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            case_insensitive: false,
            multiline: false,
            dot_all: false,
            newlines: &['\n'],
            max_depth: 1_000,
            max_stack: 1_000_000,
            max_pattern: 1_000,
            max_insts: 10_000,
        }
    }
}

/// How many matching steps the caller allows. Share one across several searches to cap them
/// together; when it runs out, the search ends in [`Error::TooComplex`].
#[derive(Clone, Debug)]
pub struct Budget {
    left: u64,
}

impl Budget {
    pub fn new(steps: u64) -> Self {
        Self { left: steps }
    }

    /// Steps not used yet.
    pub fn remaining(&self) -> u64 {
        self.left
    }

    fn spend(&mut self, steps: u64) -> Result<(), Error> {
        self.left = self.left.checked_sub(steps).ok_or(Error::TooComplex)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The pattern is malformed. `pos` counts chars from the start of the pattern.
    Syntax { pos: usize, msg: String },
    /// The pattern uses PCRE2 syntax this engine does not support.
    Unsupported(String),
    /// The pattern or search went over a budget or cap.
    TooComplex,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax { pos, msg } => write!(f, "regex syntax error at {pos}: {msg}"),
            Error::Unsupported(what) => write!(f, "unsupported regex syntax: {what}"),
            Error::TooComplex => f.write_str("regex is too complex"),
        }
    }
}

impl std::error::Error for Error {}

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Regex {
    prog: compile::Program,
    names: Arc<[(Box<str>, usize)]>,
    newlines: Box<[char]>,
    max_depth: u32,
    max_stack: usize,
}

impl Regex {
    pub fn new(pattern: &str, o: &Options) -> Result<Regex, Error> {
        let parsed = parse::parse(pattern, o)?;
        let names: Arc<[(Box<str>, usize)]> = parsed.names.iter().map(|(n, g)| (n.clone(), *g)).collect();
        let prog = compile::compile(parsed, o.max_insts)?;
        Ok(Regex { prog, names, newlines: o.newlines.into(), max_depth: o.max_depth, max_stack: o.max_stack })
    }

    /// The leftmost-first match that starts and ends inside `range` of `hay`.
    ///
    /// Text after `range.end` is not seen at all (as if `hay` ended there); text before
    /// `range.start` is seen by lookbehind, `\b` and `^`. An invalid range (reversed, out of
    /// bounds or not on char boundaries) has no match.
    pub fn captures_at(&self, hay: &str, range: Range<usize>, b: &mut Budget) -> Result<Option<Captures>, Error> {
        let Some(hay) = hay.get(..range.end) else { return Ok(None) };
        if range.start > range.end || !hay.is_char_boundary(range.start) {
            return Ok(None);
        }
        let limits = vm::Limits { max_depth: self.max_depth, max_stack: self.max_stack };
        let found = vm::search(&self.prog, &self.newlines, limits, hay, range.start, b)?;
        Ok(found.map(|slots| Captures { slots, names: self.names.clone() }))
    }

    /// Named groups as `(index, name)`, in name order.
    pub fn group_names(&self) -> impl Iterator<Item = (usize, &str)> {
        self.names.iter().map(|(n, g)| (*g, &**n))
    }

    /// Number of groups, counting group 0 (the whole match).
    pub fn captures_len(&self) -> usize {
        self.prog.groups
    }
}

/// The groups of one match. Group 0 is the whole match (starting after any `\K`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    slots: Vec<Option<usize>>,
    names: Arc<[(Box<str>, usize)]>,
}

impl Captures {
    /// Byte range of group `i`, if it took part in the match.
    pub fn get(&self, i: usize) -> Option<Range<usize>> {
        let start = (*self.slots.get(i.checked_mul(2)?)?)?;
        let end = (*self.slots.get(i.checked_mul(2)?.checked_add(1)?)?)?;
        Some(start..end)
    }

    /// Byte range of the group named `name`, if it took part in the match.
    pub fn name(&self, name: &str) -> Option<Range<usize>> {
        let (_, g) = self.names.iter().find(|(n, _)| &**n == name)?;
        self.get(*g)
    }

    /// Number of groups, counting group 0.
    pub fn len(&self) -> usize {
        self.slots.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}
