//! The parsed pattern: a flat arena of nodes. A node's children always come before it, so
//! walking the arena in order visits children first, with no recursion (and no recursive drop).
//!
//! Flags are already applied: case-insensitive literals are sets, `.` and `^`/`$` carry their
//! mode, so the compiler never sees `(?i)` or `(?m)`.

use crate::class::CharSet;

pub(crate) type NodeId = usize;

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Empty,
    Char(char),
    Set(CharSet),
    /// `.` or `\N`. `newline: true` also matches the newline characters.
    Any {
        newline: bool,
    },
    /// `\R`: `\r\n` or one vertical space char.
    LineBreak,
    Assert(Look),
    /// `\K`: the match is reported from here.
    Keep,
    Concat(Vec<NodeId>),
    Alt(Vec<NodeId>),
    Group {
        index: Option<usize>,
        body: NodeId,
    },
    Repeat {
        body: NodeId,
        min: u32,
        max: Option<u32>,
        kind: RepeatKind,
    },
    Atomic(NodeId),
    Lookaround {
        behind: bool,
        negate: bool,
        body: NodeId,
    },
    Backref {
        group: GroupRef,
        ci: bool,
    },
    Call(GroupRef),
    Cond {
        cond: Cond,
        yes: NodeId,
        no: NodeId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepeatKind {
    Greedy,
    Lazy,
    Possessive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    /// `\A`, or `^` without multiline.
    Start,
    /// `^` with multiline.
    LineStart,
    /// `\z`.
    End,
    /// `\Z`, or `$` without multiline: the end, or before a final newline.
    EndBeforeNewline,
    /// `$` with multiline.
    LineEnd,
    WordBoundary,
    NotWordBoundary,
}

/// A group named in the pattern, resolved to an index once every group is known.
#[derive(Clone, Debug)]
pub(crate) enum GroupRef {
    Index(usize),
    Name { name: String, pos: usize },
}

#[derive(Clone, Debug)]
pub(crate) enum Cond {
    Group(GroupRef),
    /// `(?(R)…)` with `None`, `(?(R1)…)` or `(?(R&name)…)` with the group.
    Recursion(Option<GroupRef>),
    /// `(?(DEFINE)…)`: never true.
    Define,
    /// `(?(?=…)…)` and the like: the node is a `Lookaround`.
    Look(NodeId),
}

#[derive(Debug)]
pub(crate) struct Ast {
    pub nodes: Vec<Node>,
    /// Number of groups, counting group 0.
    pub groups: usize,
    /// Sorted by name.
    pub names: Vec<(Box<str>, usize)>,
    /// Char position of each node in the pattern, for errors found while compiling.
    pub pos: Vec<usize>,
}
