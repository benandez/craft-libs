//! The backtracking matcher. State changes go on a heap stack as undo entries next to the
//! alternatives still to try, so failing pops back to the last alternative and restores
//! everything on the way. Atomic groups and lookarounds drop their body's alternatives at the
//! end (a cut) but keep its undo entries.

use crate::ast::Look;
use crate::class;
use crate::compile::{Inst, Program};
use crate::{Budget, Error};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_depth: u32,
    pub max_stack: usize,
}

/// A call into a group, and the registers to restore when it returns.
#[derive(Debug)]
struct Frame {
    group: usize,
    ret: usize,
    regs: Vec<Option<usize>>,
    marks: Vec<usize>,
}

impl Frame {
    fn weight(&self) -> usize {
        self.regs.len().saturating_add(self.marks.len())
    }
}

#[derive(Debug)]
enum Entry {
    Alt {
        pc: usize,
        pos: usize,
    },
    /// Retry a lookbehind body starting one char further back.
    Behind {
        pc: usize,
        pos: usize,
        left: usize,
    },
    Barrier {
        pos: usize,
        fail: Option<usize>,
    },
    Reg {
        reg: usize,
        old: Option<usize>,
    },
    Mark {
        mark: usize,
        old: usize,
    },
    CallPushed,
    CallPopped(Box<Frame>),
}

impl Entry {
    fn is_alt(&self) -> bool {
        matches!(self, Entry::Alt { .. } | Entry::Behind { .. })
    }
}

struct Machine<'a> {
    prog: &'a Program,
    newlines: &'a [char],
    hay: &'a str,
    limits: Limits,
    /// Group `g` is `regs[2g]..regs[2g+1]`; `regs[2G+g]` is where its open copy started.
    regs: Vec<Option<usize>>,
    marks: Vec<usize>,
    calls: Vec<Frame>,
    stack: Vec<Entry>,
    /// Registers held by call frames (live or in `CallPopped`), counted against `max_stack`.
    heavy: usize,
}

/// The first match starting at or after `start`, as capture registers.
pub(crate) fn search(
    prog: &Program,
    newlines: &[char],
    limits: Limits,
    hay: &str,
    start: usize,
    b: &mut Budget,
) -> Result<Option<Vec<Option<usize>>>, Error> {
    let regs = prog.groups.checked_mul(3).ok_or(Error::TooComplex)?;
    let mut m =
        Machine { prog, newlines, hay, limits, regs: vec![None; regs], marks: vec![0; prog.marks], calls: Vec::new(), stack: Vec::new(), heavy: 0 };
    let mut at = start;
    loop {
        b.spend(1)?;
        let next = m.next_char(at);
        let skip = match (&prog.first, next) {
            (Some(_), None) => return Ok(None),
            (Some(f), Some(c)) => !f.contains(c),
            (None, _) => false,
        };
        if !skip && m.run(at, b)? {
            let mut regs = std::mem::take(&mut m.regs);
            regs.truncate(prog.groups.saturating_mul(2));
            return Ok(Some(regs));
        }
        match next {
            Some(c) => at = at.saturating_add(c.len_utf8()),
            None => return Ok(None),
        }
    }
}

impl Machine<'_> {
    fn next_char(&self, pos: usize) -> Option<char> {
        match self.hay.as_bytes().get(pos) {
            Some(&b) if b.is_ascii() => Some(char::from(b)),
            _ => self.hay.get(pos..)?.chars().next(),
        }
    }

    fn prev_char(&self, pos: usize) -> Option<char> {
        self.hay.get(..pos)?.chars().next_back()
    }

    /// `pos` moved back `n` chars, if there are that many.
    fn back(&self, pos: usize, n: usize) -> Option<usize> {
        match n.checked_sub(1) {
            None => Some(pos),
            Some(k) => self.hay.get(..pos)?.char_indices().rev().nth(k).map(|(i, _)| i),
        }
    }

    fn is_newline(&self, c: Option<char>) -> bool {
        c.is_some_and(|c| self.newlines.contains(&c))
    }

    fn push(&mut self, e: Entry) -> Result<(), Error> {
        if self.stack.len().saturating_add(self.heavy) >= self.limits.max_stack {
            return Err(Error::TooComplex);
        }
        self.stack.push(e);
        Ok(())
    }

    fn set_reg(&mut self, reg: usize, v: Option<usize>) -> Result<(), Error> {
        let Some(r) = self.regs.get_mut(reg) else { return Ok(()) };
        let old = std::mem::replace(r, v);
        if old != v {
            self.push(Entry::Reg { reg, old })?;
        }
        Ok(())
    }

    fn reg(&self, reg: usize) -> Option<usize> {
        self.regs.get(reg).copied().flatten()
    }

    fn open_reg(&self, g: usize) -> usize {
        self.prog.groups.saturating_mul(2).saturating_add(g)
    }

    fn assert(&self, look: Look, pos: usize) -> bool {
        let len = self.hay.len();
        match look {
            Look::Start => pos == 0,
            Look::LineStart => pos == 0 || (pos < len && self.is_newline(self.prev_char(pos))),
            Look::End => pos == len,
            Look::EndBeforeNewline => {
                pos == len || self.next_char(pos).is_some_and(|c| self.is_newline(Some(c)) && pos.saturating_add(c.len_utf8()) == len)
            }
            Look::LineEnd => pos == len || self.is_newline(self.next_char(pos)),
            Look::WordBoundary | Look::NotWordBoundary => {
                let before = self.prev_char(pos).is_some_and(class::is_word);
                let after = self.next_char(pos).is_some_and(class::is_word);
                (before != after) == (look == Look::WordBoundary)
            }
        }
    }

    /// Whether the text of group `g` is at `pos`; the position after it if so.
    fn backref(&self, g: usize, ci: bool, pos: usize) -> Option<usize> {
        let (s, e) = (self.reg(g.checked_mul(2)?)?, self.reg(g.checked_mul(2)?.checked_add(1)?)?);
        let want = self.hay.get(s..e)?;
        let rest = self.hay.get(pos..)?;
        if !ci {
            return rest.starts_with(want).then(|| pos.saturating_add(want.len()));
        }
        let mut at = pos;
        let mut have = rest.chars();
        for w in want.chars() {
            let h = have.next()?;
            if !class::fold_eq(w, h) {
                return None;
            }
            at = at.saturating_add(h.len_utf8());
        }
        Some(at)
    }

    /// Index of the innermost barrier.
    fn barrier(&self) -> Option<usize> {
        self.stack.iter().rposition(|e| matches!(e, Entry::Barrier { .. }))
    }

    /// Drops the alternatives above the innermost barrier and the barrier itself, keeping the
    /// undo entries. Returns the barrier's position.
    fn cut(&mut self, at: usize, b: &mut Budget) -> Result<usize, Error> {
        let tail = self.stack.split_off(at);
        b.spend(u64::try_from(tail.len()).unwrap_or(u64::MAX))?;
        let mut pos = 0;
        for e in tail {
            match e {
                Entry::Barrier { pos: p, .. } => pos = p,
                e if e.is_alt() => {}
                e => self.stack.push(e),
            }
        }
        Ok(pos)
    }

    /// Undoes everything down to and including the innermost barrier; returns its position.
    fn unwind(&mut self, b: &mut Budget) -> Result<usize, Error> {
        while let Some(e) = self.stack.pop() {
            b.spend(1)?;
            if let Entry::Barrier { pos, .. } = e {
                return Ok(pos);
            }
            self.undo(e);
        }
        Ok(0)
    }

    fn undo(&mut self, e: Entry) {
        match e {
            Entry::Reg { reg, old } => {
                if let Some(r) = self.regs.get_mut(reg) {
                    *r = old;
                }
            }
            Entry::Mark { mark, old } => {
                if let Some(m) = self.marks.get_mut(mark) {
                    *m = old;
                }
            }
            Entry::CallPushed => {
                if let Some(f) = self.calls.pop() {
                    self.heavy = self.heavy.saturating_sub(f.weight());
                }
            }
            Entry::CallPopped(f) => self.calls.push(*f),
            Entry::Alt { .. } | Entry::Behind { .. } | Entry::Barrier { .. } => {}
        }
    }

    /// Pops to the next alternative; `None` when there is none left.
    fn backtrack(&mut self, b: &mut Budget) -> Result<Option<(usize, usize)>, Error> {
        while let Some(e) = self.stack.pop() {
            b.spend(1)?;
            match e {
                Entry::Alt { pc, pos } => return Ok(Some((pc, pos))),
                Entry::Behind { pc, pos, left } => {
                    let Some(p) = self.back(pos, 1) else { continue };
                    if let Some(left) = left.checked_sub(1).filter(|l| *l > 0) {
                        self.push(Entry::Behind { pc, pos: p, left })?;
                    }
                    return Ok(Some((pc, p)));
                }
                Entry::Barrier { pos, fail: Some(pc) } => return Ok(Some((pc, pos))),
                e => self.undo(e),
            }
        }
        Ok(None)
    }

    /// Runs the program anchored at `at`; on a match, `regs` holds the groups.
    fn run(&mut self, at: usize, b: &mut Budget) -> Result<bool, Error> {
        self.regs.iter_mut().for_each(|r| *r = None);
        self.calls.clear();
        self.stack.clear();
        self.heavy = 0;
        let (mut pc, mut pos) = (0usize, at);
        loop {
            b.spend(1)?;
            let next = pc.saturating_add(1);
            let ok = match self.prog.insts.get(pc).copied() {
                None => false,
                Some(Inst::Char(c)) => match self.next_char(pos) {
                    Some(h) if h == c => {
                        pos = pos.saturating_add(h.len_utf8());
                        pc = next;
                        true
                    }
                    _ => false,
                },
                Some(Inst::Set(i)) => match self.next_char(pos) {
                    Some(h) if self.prog.sets.get(i).is_some_and(|s| s.contains(h)) => {
                        pos = pos.saturating_add(h.len_utf8());
                        pc = next;
                        true
                    }
                    _ => false,
                },
                Some(inst @ (Inst::Any | Inst::AnyNoNl)) => match self.next_char(pos) {
                    Some(h) if inst == Inst::Any || !self.is_newline(Some(h)) => {
                        pos = pos.saturating_add(h.len_utf8());
                        pc = next;
                        true
                    }
                    _ => false,
                },
                Some(Inst::LineBreak) => {
                    if self.hay.get(pos..).is_some_and(|s| s.starts_with("\r\n")) {
                        pos = pos.saturating_add(2);
                        pc = next;
                        true
                    } else {
                        match self.next_char(pos) {
                            Some(h) if class::is_vertical(h) => {
                                pos = pos.saturating_add(h.len_utf8());
                                pc = next;
                                true
                            }
                            _ => false,
                        }
                    }
                }
                Some(Inst::Assert(look)) => {
                    pc = next;
                    self.assert(look, pos)
                }
                Some(Inst::Split(a, alt)) => {
                    self.push(Entry::Alt { pc: alt, pos })?;
                    pc = a;
                    true
                }
                Some(Inst::Jmp(a)) => {
                    pc = a;
                    true
                }
                Some(Inst::Open(g)) => {
                    self.set_reg(self.open_reg(g), Some(pos))?;
                    pc = next;
                    true
                }
                Some(Inst::Keep) => {
                    self.set_reg(self.open_reg(0), Some(pos))?;
                    pc = next;
                    true
                }
                Some(Inst::Close(g)) => {
                    if self.calls.last().is_some_and(|f| f.group == g) {
                        pc = self.ret(b)?;
                    } else {
                        let start = self.reg(self.open_reg(g));
                        let slot = g.saturating_mul(2);
                        self.set_reg(slot, start)?;
                        self.set_reg(slot.saturating_add(1), Some(pos))?;
                        pc = next;
                    }
                    true
                }
                Some(Inst::Backref { group, ci }) => match self.backref(group, ci, pos) {
                    Some(p) => {
                        pos = p;
                        pc = next;
                        true
                    }
                    None => false,
                },
                Some(Inst::Call(g)) => {
                    if self.calls.len() >= usize::try_from(self.limits.max_depth).unwrap_or(usize::MAX) {
                        return Err(Error::TooComplex);
                    }
                    match self.prog.starts.get(g).copied().flatten() {
                        Some(start) => {
                            let frame = Frame { group: g, ret: next, regs: self.regs.clone(), marks: self.marks.clone() };
                            self.push(Entry::CallPushed)?;
                            self.heavy = self.heavy.saturating_add(frame.weight());
                            self.calls.push(frame);
                            pc = start;
                            true
                        }
                        None => false,
                    }
                }
                Some(Inst::CondGroup { group, no }) => {
                    let set = self.reg(group.saturating_mul(2).saturating_add(1)).is_some();
                    pc = if set { next } else { no };
                    true
                }
                Some(Inst::CondRec { group, no }) => {
                    let inside = self.calls.last().is_some_and(|f| group.is_none_or(|g| f.group == g));
                    pc = if inside { next } else { no };
                    true
                }
                Some(Inst::Barrier { fail, behind }) => {
                    self.push(Entry::Barrier { pos, fail })?;
                    pc = next;
                    match behind {
                        None => true,
                        Some((lo, hi)) => match self.back(pos, lo) {
                            Some(p) => {
                                if hi > lo {
                                    self.push(Entry::Behind { pc: next, pos: p, left: hi.saturating_sub(lo) })?;
                                }
                                pos = p;
                                true
                            }
                            None => false,
                        },
                    }
                }
                Some(Inst::Cut { restore, behind }) => {
                    let at = self.barrier().unwrap_or(0);
                    let start = match self.stack.get(at) {
                        Some(Entry::Barrier { pos, .. }) => *pos,
                        _ => pos,
                    };
                    if behind && start != pos {
                        false
                    } else {
                        self.cut(at, b)?;
                        if restore {
                            pos = start;
                        }
                        pc = next;
                        true
                    }
                }
                Some(Inst::NegEnd { then }) => {
                    pos = self.unwind(b)?;
                    match then {
                        Some(t) => {
                            pc = t;
                            true
                        }
                        None => false,
                    }
                }
                Some(Inst::MarkSet(mark)) => {
                    if let Some(m) = self.marks.get_mut(mark) {
                        let old = std::mem::replace(m, pos);
                        self.push(Entry::Mark { mark, old })?;
                    }
                    pc = next;
                    true
                }
                Some(Inst::LoopBack { mark, to }) => {
                    pc = if self.marks.get(mark) == Some(&pos) { next } else { to };
                    true
                }
                Some(Inst::Match) => return Ok(true),
            };
            if !ok {
                match self.backtrack(b)? {
                    Some((p, at)) => (pc, pos) = (p, at),
                    None => return Ok(false),
                }
            }
        }
    }

    /// Returns from the innermost call: restores the registers saved at the call (PCRE2: groups
    /// set inside a recursion revert when it returns), keeping group 0.
    fn ret(&mut self, b: &mut Budget) -> Result<usize, Error> {
        let Some(frame) = self.calls.pop() else { return Ok(0) };
        b.spend(u64::try_from(frame.weight()).unwrap_or(u64::MAX))?;
        for (reg, &v) in frame.regs.iter().enumerate().skip(2) {
            self.set_reg(reg, v)?;
        }
        for (mark, &v) in frame.marks.iter().enumerate() {
            if let Some(m) = self.marks.get_mut(mark) {
                let old = std::mem::replace(m, v);
                if old != v {
                    self.push(Entry::Mark { mark, old })?;
                }
            }
        }
        let ret = frame.ret;
        self.push(Entry::CallPopped(Box::new(frame)))?;
        Ok(ret)
    }
}
