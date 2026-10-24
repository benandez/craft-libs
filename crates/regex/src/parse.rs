//! Pattern text → [`Ast`]. Open groups live on an explicit frame stack, so nesting costs heap,
//! not Rust stack; it is capped at `max_depth`.

use regex_syntax::hir::ClassUnicode;

use crate::ast::{Ast, Cond, GroupRef, Look, Node, NodeId, RepeatKind};
use crate::class::{self, CharSet};
use crate::{Error, Options};

/// Largest `{n,m}` count.
const MAX_REPEAT: u32 = 65_535;
const MAX_NAME: usize = 32;

#[derive(Clone, Copy, Debug)]
struct Flags {
    i: bool,
    m: bool,
    s: bool,
    x: bool,
    n: bool,
}

#[derive(Debug)]
enum Kind {
    Root,
    Capture(usize),
    NonCapture,
    Atomic,
    Look {
        behind: bool,
        negate: bool,
        is_cond: bool,
    },
    /// `(?|`: the group count at the start, and the most seen in any branch.
    BranchReset {
        start: usize,
        max: usize,
    },
    /// `None` until an assertion condition has been parsed.
    Cond(Option<Cond>),
}

#[derive(Debug)]
struct Frame {
    kind: Kind,
    pos: usize,
    alts: Vec<NodeId>,
    items: Vec<NodeId>,
    /// Flags to restore when the frame closes.
    outer: Flags,
}

/// What an escape stands for.
enum Esc {
    Char(char),
    Class(ClassUnicode),
    Node(Node),
    Quote,
    EndQuote,
}

pub(crate) fn parse(pattern: &str, o: &Options) -> Result<Ast, Error> {
    let chars: Vec<char> = pattern.chars().collect();
    if chars.len() > o.max_pattern {
        return Err(Error::TooComplex);
    }
    let flags = Flags { i: o.case_insensitive, m: o.multiline, s: o.dot_all, x: false, n: false };
    let mut p = Parser { chars, i: 0, nodes: Vec::new(), pos: Vec::new(), groups: 0, names: Vec::new(), flags };
    p.run(o.max_depth)
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    nodes: Vec<Node>,
    pos: Vec<usize>,
    /// Capture groups opened so far.
    groups: usize,
    names: Vec<(Box<str>, usize)>,
    flags: Flags,
}

fn syntax<T>(pos: usize, msg: &str) -> Result<T, Error> {
    Err(Error::Syntax { pos, msg: msg.to_owned() })
}

fn unsupported<T>(what: &str) -> Result<T, Error> {
    Err(Error::Unsupported(what.to_owned()))
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.chars.get(self.i.checked_add(k)?).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i = self.i.saturating_add(1);
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        let hit = self.peek() == Some(c);
        if hit {
            self.i = self.i.saturating_add(1);
        }
        hit
    }

    fn eat_str(&mut self, s: &str) -> bool {
        let hit = s.chars().enumerate().all(|(k, c)| self.peek_at(k) == Some(c));
        if hit {
            self.i = self.i.saturating_add(s.chars().count());
        }
        hit
    }

    fn expect(&mut self, c: char, msg: &str) -> Result<(), Error> {
        if self.eat(c) { Ok(()) } else { syntax(self.i, msg) }
    }

    fn add(&mut self, node: Node, pos: usize) -> NodeId {
        self.nodes.push(node);
        self.pos.push(pos);
        self.nodes.len().saturating_sub(1)
    }

    fn literal(&mut self, c: char, pos: usize) -> NodeId {
        if !self.flags.i {
            return self.add(Node::Char(c), pos);
        }
        let mut cls = class::of_char(c);
        class::fold(&mut cls);
        let set = CharSet::new(&cls);
        let node = match set.single() {
            Some(c) => Node::Char(c),
            None => Node::Set(set),
        };
        self.add(node, pos)
    }

    fn run(&mut self, max_depth: u32) -> Result<Ast, Error> {
        let max_depth = usize::try_from(max_depth).unwrap_or(usize::MAX);
        let mut frames = vec![Frame { kind: Kind::Root, pos: 0, alts: Vec::new(), items: Vec::new(), outer: self.flags }];
        let mut quoting = false;
        loop {
            if quoting {
                if self.eat_str("\\E") {
                    quoting = false;
                    continue;
                }
                let Some(c) = self.bump() else { break };
                let pos = self.i.saturating_sub(1);
                let id = self.literal(c, pos);
                top(&mut frames)?.items.push(id);
                continue;
            }
            if self.flags.x {
                self.skip_extended();
            }
            let pos = self.i;
            let Some(c) = self.bump() else { break };
            match c {
                '(' => {
                    self.open(&mut frames, pos)?;
                    if frames.len() > max_depth {
                        return Err(Error::TooComplex);
                    }
                }
                ')' => {
                    if frames.len() <= 1 {
                        return syntax(pos, "unmatched closing parenthesis");
                    }
                    let Some(f) = frames.pop() else { return syntax(pos, "unmatched closing parenthesis") };
                    self.close(f, &mut frames)?;
                }
                '|' => {
                    let groups = self.groups;
                    let f = top(&mut frames)?;
                    let items = std::mem::take(&mut f.items);
                    let pos = f.pos;
                    let id = self.concat(items, pos);
                    let f = top(&mut frames)?;
                    f.alts.push(id);
                    if let Kind::BranchReset { start, max } = &mut f.kind {
                        *max = (*max).max(groups);
                        self.groups = *start;
                    }
                }
                '*' => self.repeat(&mut frames, 0, None, pos)?,
                '+' => self.repeat(&mut frames, 1, None, pos)?,
                '?' => self.repeat(&mut frames, 0, Some(1), pos)?,
                '{' => match self.counted()? {
                    Some((min, max)) => self.repeat(&mut frames, min, max, pos)?,
                    None => {
                        let id = self.literal('{', pos);
                        top(&mut frames)?.items.push(id);
                    }
                },
                '^' => {
                    let look = if self.flags.m { Look::LineStart } else { Look::Start };
                    let id = self.add(Node::Assert(look), pos);
                    top(&mut frames)?.items.push(id);
                }
                '$' => {
                    let look = if self.flags.m { Look::LineEnd } else { Look::EndBeforeNewline };
                    let id = self.add(Node::Assert(look), pos);
                    top(&mut frames)?.items.push(id);
                }
                '.' => {
                    let id = self.add(Node::Any { newline: self.flags.s }, pos);
                    top(&mut frames)?.items.push(id);
                }
                '[' => {
                    let set = self.class(pos)?;
                    let id = self.add(Node::Set(set), pos);
                    top(&mut frames)?.items.push(id);
                }
                '\\' => {
                    let node = match self.escape(false)? {
                        Esc::Char(c) => {
                            let id = self.literal(c, pos);
                            top(&mut frames)?.items.push(id);
                            continue;
                        }
                        Esc::Class(mut cls) => {
                            if self.flags.i {
                                class::fold(&mut cls);
                            }
                            Node::Set(CharSet::new(&cls))
                        }
                        Esc::Node(Node::Keep) if frames.iter().any(|f| matches!(f.kind, Kind::Look { .. })) => {
                            return syntax(pos, "\\K is not allowed in lookaround");
                        }
                        Esc::Node(n) => n,
                        Esc::Quote => {
                            quoting = true;
                            continue;
                        }
                        Esc::EndQuote => continue,
                    };
                    let id = self.add(node, pos);
                    top(&mut frames)?.items.push(id);
                }
                c => {
                    let id = self.literal(c, pos);
                    top(&mut frames)?.items.push(id);
                }
            }
        }
        if frames.len() != 1 {
            let pos = frames.last().map_or(0, |f| f.pos);
            return syntax(pos, "missing closing parenthesis");
        }
        let Some(root) = frames.pop() else { return syntax(0, "empty pattern stack") };
        let body = self.alternation(root.alts, root.items, 0);
        debug_assert_eq!(body, self.nodes.len().saturating_sub(1));
        self.names.sort();
        Ok(Ast {
            nodes: std::mem::take(&mut self.nodes),
            groups: self.groups.saturating_add(1),
            names: std::mem::take(&mut self.names),
            pos: std::mem::take(&mut self.pos),
        })
    }

    fn skip_extended(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i = self.i.saturating_add(1);
            } else if c == '#' {
                while let Some(c) = self.bump() {
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn concat(&mut self, mut items: Vec<NodeId>, pos: usize) -> NodeId {
        match items.len() {
            0 => self.add(Node::Empty, pos),
            1 => items.pop().unwrap_or(0),
            _ => self.add(Node::Concat(items), pos),
        }
    }

    fn alternation(&mut self, mut alts: Vec<NodeId>, items: Vec<NodeId>, pos: usize) -> NodeId {
        let last = self.concat(items, pos);
        if alts.is_empty() {
            return last;
        }
        alts.push(last);
        self.add(Node::Alt(alts), pos)
    }

    /// `{n}`, `{n,}` or `{n,m}` after the `{`; `None` (and nothing consumed) if it isn't one.
    fn counted(&mut self) -> Result<Option<(u32, Option<u32>)>, Error> {
        let start = self.i;
        let Some(min) = self.number()? else { return Ok(None) };
        let max = if self.eat(',') { self.number()? } else { Some(min) };
        if !self.eat('}') {
            self.i = start;
            return Ok(None);
        }
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
            return syntax(start, "number too big in {} quantifier");
        }
        if max.is_some_and(|m| m < min) {
            return syntax(start, "numbers out of order in {} quantifier");
        }
        Ok(Some((min, max)))
    }

    /// Decimal digits; `None` if there are none.
    fn number(&mut self) -> Result<Option<u32>, Error> {
        let start = self.i;
        let mut n: Option<u32> = None;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            self.i = self.i.saturating_add(1);
            let v = n.unwrap_or(0).checked_mul(10).and_then(|v| v.checked_add(d));
            n = Some(v.map_or_else(|| syntax(start, "number is too big"), Ok)?);
        }
        Ok(n)
    }

    fn repeat(&mut self, frames: &mut [Frame], min: u32, max: Option<u32>, pos: usize) -> Result<(), Error> {
        let kind = if self.eat('?') {
            RepeatKind::Lazy
        } else if self.eat('+') {
            RepeatKind::Possessive
        } else {
            RepeatKind::Greedy
        };
        let f = top(frames)?;
        let Some(body) = f.items.pop() else { return syntax(pos, "quantifier does not follow a repeatable item") };
        let id = self.add(Node::Repeat { body, min, max, kind }, pos);
        top(frames)?.items.push(id);
        Ok(())
    }

    fn name(&mut self, end: char) -> Result<String, Error> {
        let start = self.i;
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c == end {
                break;
            }
            name.push(c);
            self.i = self.i.saturating_add(1);
        }
        self.expect(end, "missing terminator for group name")?;
        let mut cs = name.chars();
        let ok = cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name.len() <= MAX_NAME;
        if !ok {
            return syntax(start, "invalid group name");
        }
        Ok(name)
    }

    /// A group number: `n`, `+n` or `-n`, relative to the groups opened so far.
    fn group_number(&mut self, pos: usize) -> Result<Option<usize>, Error> {
        let sign = match self.peek() {
            Some('+') => 1,
            Some('-') => -1,
            _ => 0,
        };
        let start = self.i;
        if sign != 0 {
            self.i = self.i.saturating_add(1);
        }
        let Some(n) = self.number()? else {
            self.i = start;
            return Ok(None);
        };
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        let index = match sign {
            0 => Some(n),
            1 => self.groups.checked_add(n),
            _ => self.groups.checked_add(1).and_then(|g| g.checked_sub(n)),
        };
        match index {
            Some(i) if sign == 0 || (n > 0 && i > 0) => Ok(Some(i)),
            _ => syntax(pos, "reference to non-existent group"),
        }
    }

    fn capture(&mut self, name: Option<String>, pos: usize) -> Result<Kind, Error> {
        self.groups = self.groups.checked_add(1).ok_or(Error::TooComplex)?;
        if let Some(name) = name {
            match self.names.iter().find(|(n, _)| **n == *name) {
                Some((_, g)) if *g != self.groups => return syntax(pos, "two named groups have the same name"),
                Some(_) => {}
                None => self.names.push((name.into(), self.groups)),
            }
        }
        Ok(Kind::Capture(self.groups))
    }

    fn frame(&self, kind: Kind, pos: usize) -> Frame {
        Frame { kind, pos, alts: Vec::new(), items: Vec::new(), outer: self.flags }
    }

    /// After `(`: pushes the frames the group opens (none for flags, calls, comments, `(?P=n)`).
    fn open(&mut self, frames: &mut Vec<Frame>, pos: usize) -> Result<(), Error> {
        if self.peek() == Some('*') {
            return unsupported("(*VERB)");
        }
        if !self.eat('?') {
            let kind = if self.flags.n { Kind::NonCapture } else { self.capture(None, pos)? };
            frames.push(self.frame(kind, pos));
            return Ok(());
        }
        let look = |behind, negate| Kind::Look { behind, negate, is_cond: false };
        let kind = if self.eat(':') {
            Kind::NonCapture
        } else if self.eat('|') {
            Kind::BranchReset { start: self.groups, max: self.groups }
        } else if self.eat('>') {
            Kind::Atomic
        } else if self.eat('=') {
            look(false, false)
        } else if self.eat('!') {
            look(false, true)
        } else if self.eat_str("<=") {
            look(true, false)
        } else if self.eat_str("<!") {
            look(true, true)
        } else if self.eat('<') {
            let name = self.name('>')?;
            self.capture(Some(name), pos)?
        } else if self.eat('\'') {
            let name = self.name('\'')?;
            self.capture(Some(name), pos)?
        } else if self.eat_str("P<") {
            let name = self.name('>')?;
            self.capture(Some(name), pos)?
        } else if self.eat_str("P=") {
            let name = self.name(')')?;
            self.push(frames, Node::Backref { group: GroupRef::Name { name, pos }, ci: self.flags.i }, pos)?;
            return Ok(());
        } else if self.eat_str("P>") || self.eat('&') {
            let name = self.name(')')?;
            self.push(frames, Node::Call(GroupRef::Name { name, pos }), pos)?;
            return Ok(());
        } else if self.eat('R') {
            self.expect(')', "(?R must be followed by )")?;
            self.push(frames, Node::Call(GroupRef::Index(0)), pos)?;
            return Ok(());
        } else if let Some(g) = self.group_number(pos)? {
            self.expect(')', "missing ) after group number")?;
            self.push(frames, Node::Call(GroupRef::Index(g)), pos)?;
            return Ok(());
        } else if self.eat('#') {
            while let Some(c) = self.bump() {
                if c == ')' {
                    return Ok(());
                }
            }
            return syntax(pos, "missing ) after comment");
        } else if self.eat('(') {
            return self.condition(frames, pos);
        } else if self.eat('C') {
            return unsupported("callout (?C)");
        } else {
            if let Some(f) = self.flag_group(pos)? {
                frames.push(f);
            }
            return Ok(());
        };
        frames.push(self.frame(kind, pos));
        Ok(())
    }

    fn push(&mut self, frames: &mut [Frame], node: Node, pos: usize) -> Result<(), Error> {
        let id = self.add(node, pos);
        top(frames)?.items.push(id);
        Ok(())
    }

    /// `(?imsxn-imsxn)` or `(?imsxn-imsxn:`.
    fn flag_group(&mut self, pos: usize) -> Result<Option<Frame>, Error> {
        let mut on = true;
        let mut flags = self.flags;
        loop {
            let Some(c) = self.bump() else { return syntax(pos, "missing ) after (? flags") };
            match c {
                'i' => flags.i = on,
                'm' => flags.m = on,
                's' => flags.s = on,
                'x' => flags.x = on,
                'n' => flags.n = on,
                '-' if on => on = false,
                'U' | 'J' | 'X' => return unsupported(&format!("(?{c})")),
                '^' => return unsupported("(?^)"),
                ')' => {
                    self.flags = flags;
                    return Ok(None);
                }
                ':' => {
                    let f = self.frame(Kind::NonCapture, pos);
                    self.flags = flags;
                    return Ok(Some(f));
                }
                _ => return syntax(self.i.saturating_sub(1), "unrecognized character after (? or (?-"),
            }
        }
    }

    /// After `(?(`.
    fn condition(&mut self, frames: &mut Vec<Frame>, pos: usize) -> Result<(), Error> {
        if self.peek() == Some('?') {
            let negate = match (self.peek_at(1), self.peek_at(2)) {
                (Some('='), _) => Some((false, false, 2)),
                (Some('!'), _) => Some((false, true, 2)),
                (Some('<'), Some('=')) => Some((true, false, 3)),
                (Some('<'), Some('!')) => Some((true, true, 3)),
                _ => None,
            };
            let Some((behind, negate, len)) = negate else { return syntax(pos, "assertion expected after (?(") };
            let look_pos = self.i.saturating_sub(1);
            self.i = self.i.saturating_add(len);
            frames.push(self.frame(Kind::Cond(None), pos));
            frames.push(self.frame(Kind::Look { behind, negate, is_cond: true }, look_pos));
            return Ok(());
        }
        let cond = if self.eat('<') {
            let name = self.name('>')?;
            self.expect(')', "missing ) after (?(<name>")?;
            Cond::Group(GroupRef::Name { name, pos })
        } else if self.eat('\'') {
            let name = self.name('\'')?;
            self.expect(')', "missing ) after (?('name'")?;
            Cond::Group(GroupRef::Name { name, pos })
        } else if self.eat_str("R&") {
            Cond::Recursion(Some(GroupRef::Name { name: self.name(')')?, pos }))
        } else if self.eat_str("DEFINE)") {
            Cond::Define
        } else if self.peek() == Some('R') && self.peek_at(1).is_some_and(|c| c == ')' || c.is_ascii_digit()) {
            self.i = self.i.saturating_add(1);
            let g = self.number()?.map(|n| GroupRef::Index(usize::try_from(n).unwrap_or(usize::MAX)));
            self.expect(')', "malformed (?(R condition")?;
            Cond::Recursion(g)
        } else if let Some(g) = self.group_number(pos)? {
            self.expect(')', "malformed number after (?(")?;
            Cond::Group(GroupRef::Index(g))
        } else {
            Cond::Group(GroupRef::Name { name: self.name(')')?, pos })
        };
        frames.push(self.frame(Kind::Cond(Some(cond)), pos));
        Ok(())
    }

    fn close(&mut self, f: Frame, frames: &mut [Frame]) -> Result<(), Error> {
        let pos = f.pos;
        self.flags = f.outer;
        if let Kind::Cond(cond) = f.kind {
            let Some(cond) = cond else { return syntax(pos, "assertion expected after (?(") };
            if f.alts.len() > 1 {
                return syntax(pos, "conditional group contains more than two branches");
            }
            let mut alts = f.alts;
            let last = self.concat(f.items, pos);
            let (yes, no) = match alts.pop() {
                Some(yes) => (yes, last),
                None => (last, self.add(Node::Empty, pos)),
            };
            return self.push(frames, Node::Cond { cond, yes, no }, pos);
        }
        let groups = self.groups;
        let body = self.alternation(f.alts, f.items, pos);
        let node = match f.kind {
            Kind::Capture(index) => Node::Group { index: Some(index), body },
            Kind::Atomic => Node::Atomic(body),
            Kind::Look { behind, negate, is_cond: true } => {
                let id = self.add(Node::Lookaround { behind, negate, body }, pos);
                let parent = top(frames)?;
                parent.kind = Kind::Cond(Some(Cond::Look(id)));
                return Ok(());
            }
            Kind::Look { behind, negate, .. } => Node::Lookaround { behind, negate, body },
            Kind::BranchReset { max, .. } => {
                self.groups = max.max(groups);
                Node::Group { index: None, body }
            }
            Kind::NonCapture | Kind::Root | Kind::Cond(_) => Node::Group { index: None, body },
        };
        self.push(frames, node, pos)
    }

    /// A `[…]` class, after the `[`.
    fn class(&mut self, pos: usize) -> Result<CharSet, Error> {
        let negate = self.eat('^');
        let mut cls = ClassUnicode::empty();
        let mut first = true;
        loop {
            let Some(c) = self.bump() else { return syntax(pos, "missing terminating ] for character class") };
            if c == ']' && !first {
                break;
            }
            first = false;
            if c == '['
                && self.peek() == Some(':')
                && let Some(p) = self.posix()?
            {
                cls.union(&p);
                continue;
            }
            let lo = match c {
                '\\' => match self.escape(true)? {
                    Esc::Char(c) => c,
                    Esc::Class(p) => {
                        cls.union(&p);
                        continue;
                    }
                    Esc::EndQuote => continue,
                    Esc::Quote => return unsupported("\\Q inside a character class"),
                    Esc::Node(_) => return syntax(self.i, "escape not allowed in a character class"),
                },
                c => c,
            };
            let hi = if self.peek() == Some('-') && self.peek_at(1).is_some_and(|c| c != ']') {
                self.i = self.i.saturating_add(1);
                let hi_pos = self.i;
                match self.bump() {
                    Some('\\') => match self.escape(true)? {
                        Esc::Char(c) => c,
                        _ => return syntax(hi_pos, "invalid range in character class"),
                    },
                    Some('[') if self.peek() == Some(':') => {
                        return syntax(hi_pos, "invalid range in character class");
                    }
                    Some(c) => c,
                    None => return syntax(pos, "missing terminating ] for character class"),
                }
            } else {
                lo
            };
            if hi < lo {
                return syntax(self.i, "range out of order in character class");
            }
            cls.union(&class::of_ranges(&[(lo, hi)]));
        }
        if self.flags.i {
            class::fold(&mut cls);
        }
        if negate {
            cls.negate();
        }
        Ok(CharSet::new(&cls))
    }

    /// `[:name:]` after the `[`; `None` (nothing consumed) if it isn't one.
    fn posix(&mut self) -> Result<Option<ClassUnicode>, Error> {
        let start = self.i;
        self.i = self.i.saturating_add(1);
        let negate = self.eat('^');
        let mut name = String::new();
        while let Some(c) = self.peek().filter(char::is_ascii_lowercase) {
            name.push(c);
            self.i = self.i.saturating_add(1);
        }
        if !self.eat_str(":]") {
            self.i = start;
            return Ok(None);
        }
        let Some(mut cls) = class::posix(&name) else { return syntax(start, "unknown POSIX class name") };
        if negate {
            cls.negate();
        }
        Ok(Some(cls))
    }

    /// After a `\`.
    fn escape(&mut self, in_class: bool) -> Result<Esc, Error> {
        let pos = self.i.saturating_sub(1);
        let Some(c) = self.bump() else { return syntax(pos, "\\ at end of pattern") };
        let node = |n: Node| if in_class { syntax(pos, "escape not allowed in a character class") } else { Ok(Esc::Node(n)) };
        let ch = |c: char| Ok(Esc::Char(c));
        match c {
            't' => ch('\t'),
            'n' => ch('\n'),
            'r' => ch('\r'),
            'e' => ch('\x1b'),
            'f' => ch('\x0c'),
            'a' => ch('\x07'),
            'b' if in_class => ch('\x08'),
            'x' => {
                if self.eat('{') {
                    self.code_point(16, '}', pos).map(Esc::Char)
                } else {
                    let mut v = 0u32;
                    for _ in 0..2 {
                        let Some(d) = self.peek().and_then(|c| c.to_digit(16)) else { break };
                        self.i = self.i.saturating_add(1);
                        v = v.saturating_mul(16).saturating_add(d);
                    }
                    char::from_u32(v).map_or_else(|| syntax(pos, "invalid code point"), ch)
                }
            }
            'o' => {
                self.expect('{', "missing { after \\o")?;
                self.code_point(8, '}', pos).map(Esc::Char)
            }
            '0' => {
                let mut v = 0u32;
                for _ in 0..2 {
                    let Some(d) = self.peek().and_then(|c| c.to_digit(8)) else { break };
                    self.i = self.i.saturating_add(1);
                    v = v.saturating_mul(8).saturating_add(d);
                }
                char::from_u32(v).map_or_else(|| syntax(pos, "invalid code point"), ch)
            }
            'c' => match self.bump() {
                Some(x) if x.is_ascii_graphic() || x == ' ' => {
                    let v = u32::from(x.to_ascii_uppercase()) ^ 0x40;
                    char::from_u32(v).map_or_else(|| syntax(pos, "invalid \\c"), ch)
                }
                _ => syntax(pos, "\\c must be followed by a printable ASCII character"),
            },
            '1'..='9' => {
                if in_class {
                    return syntax(pos, "backreference in a character class");
                }
                self.i = pos.saturating_add(1);
                let n = self.number()?.unwrap_or(0);
                let index = usize::try_from(n).unwrap_or(usize::MAX);
                // PCRE2: `\10` and up are octal when there aren't that many groups yet.
                if index >= 10 && index > self.groups && c <= '7' {
                    self.i = pos.saturating_add(1);
                    let mut v = 0u32;
                    for _ in 0..3 {
                        let Some(d) = self.peek().and_then(|c| c.to_digit(8)) else { break };
                        self.i = self.i.saturating_add(1);
                        v = v.saturating_mul(8).saturating_add(d);
                    }
                    return char::from_u32(v).map_or_else(|| syntax(pos, "invalid code point"), ch);
                }
                Ok(Esc::Node(Node::Backref { group: GroupRef::Index(index), ci: self.flags.i }))
            }
            'g' => {
                if in_class {
                    return syntax(pos, "\\g in a character class");
                }
                let call = |p: &mut Self, end| -> Result<Esc, Error> {
                    let group = match p.group_number(pos)? {
                        Some(g) => {
                            p.expect(end, "missing terminator in \\g subroutine call")?;
                            GroupRef::Index(g)
                        }
                        None => GroupRef::Name { name: p.name(end)?, pos },
                    };
                    Ok(Esc::Node(Node::Call(group)))
                };
                if self.eat('<') {
                    return call(self, '>');
                }
                if self.eat('\'') {
                    return call(self, '\'');
                }
                let braced = self.eat('{');
                let group = match self.group_number(pos)? {
                    Some(g) => GroupRef::Index(g),
                    None if braced => GroupRef::Name { name: self.name('}')?, pos },
                    None => return syntax(pos, "\\g must be followed by a number or a name"),
                };
                if braced && matches!(group, GroupRef::Index(_)) {
                    self.expect('}', "missing } after \\g{")?;
                }
                if matches!(group, GroupRef::Index(0)) {
                    return syntax(pos, "a backreference cannot refer to group 0");
                }
                Ok(Esc::Node(Node::Backref { group, ci: self.flags.i }))
            }
            'k' => {
                let end = match self.bump() {
                    Some('<') => '>',
                    Some('\'') => '\'',
                    Some('{') => '}',
                    _ => return syntax(pos, "\\k must be followed by <name>, 'name' or {name}"),
                };
                let name = self.name(end)?;
                node(Node::Backref { group: GroupRef::Name { name, pos }, ci: self.flags.i })
            }
            'd' | 'w' | 's' | 'h' | 'v' | 'D' | 'W' | 'S' | 'H' | 'V' => {
                let Some(mut cls) = class::perl(c.to_ascii_lowercase()) else { return syntax(pos, "missing class table") };
                if c.is_ascii_uppercase() {
                    cls.negate();
                }
                Ok(Esc::Class(cls))
            }
            'p' | 'P' => {
                let name = if self.eat('{') {
                    let mut name = String::new();
                    loop {
                        match self.bump() {
                            Some('}') => break,
                            Some(c) => name.push(c),
                            None => return syntax(pos, "missing } after \\p{"),
                        }
                    }
                    name
                } else {
                    self.bump().map(String::from).unwrap_or_default()
                };
                let (negate, name) = match name.strip_prefix('^') {
                    Some(rest) => (c == 'p', rest),
                    None => (c == 'P', name.as_str()),
                };
                let Some(mut cls) = class::property(name) else { return syntax(pos, "unknown property name after \\p") };
                if negate {
                    cls.negate();
                }
                Ok(Esc::Class(cls))
            }
            'R' => node(Node::LineBreak),
            'N' => node(Node::Any { newline: false }),
            'b' => node(Node::Assert(Look::WordBoundary)),
            'B' => node(Node::Assert(Look::NotWordBoundary)),
            'A' => node(Node::Assert(Look::Start)),
            'z' => node(Node::Assert(Look::End)),
            'Z' => node(Node::Assert(Look::EndBeforeNewline)),
            'K' => node(Node::Keep),
            'Q' => Ok(Esc::Quote),
            'E' => Ok(Esc::EndQuote),
            'G' | 'C' | 'X' => unsupported(&format!("\\{c}")),
            c if c.is_ascii_alphanumeric() => syntax(pos, "unrecognized escape sequence"),
            c => ch(c),
        }
    }

    /// Digits in `radix` up to `end`, as a char.
    fn code_point(&mut self, radix: u32, end: char, pos: usize) -> Result<char, Error> {
        let mut v: u32 = 0;
        let mut any = false;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) {
            self.i = self.i.saturating_add(1);
            any = true;
            v = v.checked_mul(radix).and_then(|v| v.checked_add(d)).map_or_else(|| syntax(pos, "code point is too big"), Ok)?;
        }
        if !any || !self.eat(end) {
            return syntax(pos, "malformed code point escape");
        }
        char::from_u32(v).map_or_else(|| syntax(pos, "invalid code point"), Ok)
    }
}

fn top(frames: &mut [Frame]) -> Result<&mut Frame, Error> {
    frames.last_mut().ok_or(Error::Syntax { pos: 0, msg: "empty pattern stack".to_owned() })
}
