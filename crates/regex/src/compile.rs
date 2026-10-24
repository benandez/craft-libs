//! [`Ast`] → [`Program`], in two passes with no recursion:
//!
//! 1. In arena order (children before parents): what each node can match (length bounds and
//!    first chars), group references resolved, lookbehind lengths checked.
//! 2. Top-down from the root on a work stack: emit instructions. Jump targets are labels while
//!    emitting and become pcs at the end, so each node's code is written once per use, and the
//!    work is linear in the program size (which is capped).

use regex_syntax::hir::ClassUnicode;

use crate::Error;
use crate::ast::{Ast, Cond, GroupRef, Look, Node, NodeId, RepeatKind};
use crate::class::{self, CharSet};

/// Longest lookbehind, in chars.
const MAX_LOOKBEHIND: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    Char(char),
    Set(usize),
    /// Any char, newlines included.
    Any,
    /// Any char but the newlines.
    AnyNoNl,
    LineBreak,
    Assert(Look),
    /// Try the first target; on backtrack, the second.
    Split(usize, usize),
    Jmp(usize),
    Open(usize),
    /// Ends group `g`, or returns if this is the end of a call into `g`.
    Close(usize),
    Keep,
    Backref {
        group: usize,
        ci: bool,
    },
    Call(usize),
    /// Continue if the group is set, else jump to `no`.
    CondGroup {
        group: usize,
        no: usize,
    },
    /// Continue if inside a recursion (into `group`, if given), else jump to `no`.
    CondRec {
        group: Option<usize>,
        no: usize,
    },
    /// Start of an atomic group or lookaround. If the body fails, resume at `fail` (or keep
    /// failing). `behind` is the lookbehind length range in chars.
    Barrier {
        fail: Option<usize>,
        behind: Option<(usize, usize)>,
    },
    /// End of an atomic group or positive lookaround: drop the body's alternatives.
    Cut {
        restore: bool,
        behind: bool,
    },
    /// End of a negative lookaround: the body matched (for a lookbehind, if it ends where the
    /// barrier started), so undo it, then jump to `then` (or fail).
    NegEnd {
        then: Option<usize>,
        behind: bool,
    },
    MarkSet(usize),
    /// Loop back to `to` unless the iteration matched nothing.
    LoopBack {
        mark: usize,
        to: usize,
    },
    Match,
}

impl Inst {
    /// The same instruction with every jump target passed through `f`.
    fn map(self, f: impl Fn(usize) -> usize) -> Inst {
        match self {
            Inst::Split(a, b) => Inst::Split(f(a), f(b)),
            Inst::Jmp(a) => Inst::Jmp(f(a)),
            Inst::CondGroup { group, no } => Inst::CondGroup { group, no: f(no) },
            Inst::CondRec { group, no } => Inst::CondRec { group, no: f(no) },
            Inst::Barrier { fail, behind } => Inst::Barrier { fail: fail.map(&f), behind },
            Inst::NegEnd { then, behind } => Inst::NegEnd { then: then.map(&f), behind },
            Inst::LoopBack { mark, to } => Inst::LoopBack { mark, to: f(to) },
            other => other,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<CharSet>,
    /// The pc of each group's first `Open`, for calls.
    pub starts: Vec<Option<usize>>,
    /// Number of groups, counting group 0.
    pub groups: usize,
    pub marks: usize,
    /// The chars a match can start with, if known (and the pattern can't match empty).
    pub first: Option<CharSet>,
}

/// What the compiler knows about a node's matches.
#[derive(Clone, Debug)]
struct Info {
    min: usize,
    /// `None` if unbounded or unknown.
    max: Option<usize>,
    /// The chars a non-empty match can start with; `None` if unknown.
    first: Option<ClassUnicode>,
    /// The resolved group of a backreference, call or group condition.
    group: Option<usize>,
}

fn fixed(n: usize, first: Option<ClassUnicode>) -> Info {
    Info { min: n, max: Some(n), first, group: None }
}

fn union(a: Option<ClassUnicode>, b: &Option<ClassUnicode>) -> Option<ClassUnicode> {
    let (mut a, b) = (a?, b.as_ref()?);
    a.union(b);
    Some(a)
}

fn either(a: &Info, b: &Info) -> Info {
    Info { min: a.min.min(b.min), max: a.max.zip(b.max).map(|(a, b)| a.max(b)), first: union(a.first.clone(), &b.first), group: None }
}

pub(crate) fn compile(ast: Ast, max_insts: usize) -> Result<Program, Error> {
    let infos = analyse(&ast)?;
    let root = ast.nodes.len().checked_sub(1);
    let mut e = Emitter {
        nodes: &ast.nodes,
        infos: &infos,
        insts: Vec::new(),
        labels: Vec::new(),
        sets: Vec::new(),
        set_of: vec![None; ast.nodes.len()],
        marks: 0,
        max_insts,
    };
    e.emit(root)?;
    let Emitter { insts, labels, sets, marks, .. } = e;
    let insts: Vec<Inst> = insts.into_iter().map(|i| i.map(|l| labels.get(l).copied().unwrap_or(usize::MAX))).collect();
    let mut starts = vec![None; ast.groups];
    for (pc, inst) in insts.iter().enumerate() {
        if let Inst::Open(g) = *inst
            && let Some(s @ None) = starts.get_mut(g)
        {
            *s = Some(pc);
        }
    }
    let first = root.and_then(|r| infos.get(r)).filter(|i| i.min > 0).and_then(|i| i.first.as_ref()).map(CharSet::new);
    Ok(Program { insts, sets, starts, groups: ast.groups, marks, first })
}

/// Pass 1: facts about every node, children first.
fn analyse(ast: &Ast) -> Result<Vec<Info>, Error> {
    let at = |id: usize| ast.pos.get(id).copied().unwrap_or(0);
    let resolve = |r: &GroupRef, id: usize| -> Result<usize, Error> {
        let g = match r {
            GroupRef::Index(g) => *g,
            GroupRef::Name { name, pos } => {
                let found = ast.names.iter().find(|(n, _)| **n == **name);
                let Some((_, g)) = found else {
                    return Err(Error::Syntax { pos: *pos, msg: format!("reference to non-existent group name '{name}'") });
                };
                *g
            }
        };
        if g < ast.groups { Ok(g) } else { Err(Error::Syntax { pos: at(id), msg: "reference to non-existent group".to_owned() }) }
    };
    let mut infos: Vec<Info> = Vec::with_capacity(ast.nodes.len());
    for (id, node) in ast.nodes.iter().enumerate() {
        let info = |i: NodeId| infos.get(i).cloned().unwrap_or_else(|| fixed(0, None));
        let empty = || fixed(0, Some(ClassUnicode::empty()));
        let unknown = |group| Info { min: 0, max: None, first: None, group: Some(group) };
        let got = match node {
            Node::Empty | Node::Assert(_) | Node::Keep | Node::Lookaround { behind: false, .. } => empty(),
            Node::Char(c) => fixed(1, Some(class::of_char(*c))),
            Node::Set(set) => fixed(1, Some(set.class())),
            Node::Any { .. } => fixed(1, None),
            Node::LineBreak => Info { min: 1, max: Some(2), first: class::perl('v'), group: None },
            Node::Concat(items) => {
                let mut acc = empty();
                let mut open = true;
                for &i in items {
                    let it = info(i);
                    if open {
                        acc.first = union(acc.first, &it.first);
                        open = it.min == 0;
                    }
                    acc.min = acc.min.saturating_add(it.min);
                    acc.max = acc.max.zip(it.max).and_then(|(a, b)| a.checked_add(b));
                }
                acc
            }
            Node::Alt(alts) => {
                let mut it = alts.iter().map(|&i| info(i));
                let first = it.next().unwrap_or_else(empty);
                it.fold(first, |a, b| either(&a, &b))
            }
            Node::Group { body, .. } | Node::Atomic(body) => Info { group: None, ..info(*body) },
            Node::Repeat { body, min, max, .. } => {
                let b = info(*body);
                let (n, m) = (usize::try_from(*min).unwrap_or(usize::MAX), max.map(|m| usize::try_from(m).unwrap_or(usize::MAX)));
                Info {
                    min: b.min.saturating_mul(n),
                    max: m.and_then(|m| b.max.and_then(|x| x.checked_mul(m))),
                    first: if m == Some(0) { Some(ClassUnicode::empty()) } else { b.first },
                    group: None,
                }
            }
            Node::Lookaround { behind: true, body, .. } => {
                if !info(*body).max.is_some_and(|m| m <= MAX_LOOKBEHIND) {
                    return Err(Error::Syntax { pos: at(id), msg: "lookbehind must have a bounded length".to_owned() });
                }
                empty()
            }
            Node::Backref { group, .. } => {
                let g = resolve(group, id)?;
                if g == 0 {
                    return Err(Error::Syntax { pos: at(id), msg: "a backreference cannot refer to group 0".to_owned() });
                }
                unknown(g)
            }
            Node::Call(group) => unknown(resolve(group, id)?),
            Node::Cond { cond, yes, no } => {
                let group = match cond {
                    Cond::Group(g) | Cond::Recursion(Some(g)) => Some(resolve(g, id)?),
                    Cond::Recursion(None) | Cond::Define | Cond::Look(_) => None,
                };
                Info { group, ..either(&info(*yes), &info(*no)) }
            }
        };
        infos.push(got);
    }
    Ok(infos)
}

/// One item of pass 2's work stack.
enum Work {
    Node(NodeId),
    Inst(Inst),
    /// Pin a label to the next pc.
    Label(usize),
}

struct Emitter<'a> {
    nodes: &'a [Node],
    infos: &'a [Info],
    /// Jump targets are labels until the end.
    insts: Vec<Inst>,
    labels: Vec<usize>,
    sets: Vec<CharSet>,
    /// Each `Set` node's index in `sets`, so unrolled copies share one.
    set_of: Vec<Option<usize>>,
    marks: usize,
    max_insts: usize,
}

impl Emitter<'_> {
    fn label(&mut self) -> usize {
        self.labels.push(usize::MAX);
        self.labels.len().saturating_sub(1)
    }

    fn emit(&mut self, root: Option<NodeId>) -> Result<(), Error> {
        let mut work = vec![Work::Inst(Inst::Match), Work::Inst(Inst::Close(0))];
        work.extend(root.map(Work::Node));
        work.push(Work::Inst(Inst::Open(0)));
        while let Some(w) = work.pop() {
            match w {
                Work::Inst(i) => {
                    if self.insts.len() >= self.max_insts {
                        return Err(Error::TooComplex);
                    }
                    self.insts.push(i);
                }
                Work::Label(l) => {
                    if let Some(slot) = self.labels.get_mut(l) {
                        *slot = self.insts.len();
                    }
                }
                Work::Node(id) => {
                    // Pushed in reverse: the last item pushed is emitted first.
                    let mut seq = self.node(id)?;
                    seq.reverse();
                    work.append(&mut seq);
                }
            }
        }
        Ok(())
    }

    /// The code for one node, in order, with its children still to expand.
    fn node(&mut self, id: NodeId) -> Result<Vec<Work>, Error> {
        use Work::{Inst as I, Label as L, Node as N};
        let Some(node) = self.nodes.get(id) else { return Ok(Vec::new()) };
        let group = self.infos.get(id).and_then(|i| i.group).unwrap_or(0);
        Ok(match node {
            Node::Empty => Vec::new(),
            Node::Char(c) => vec![I(Inst::Char(*c))],
            Node::Set(set) => {
                let i = match self.set_of.get(id).copied().flatten() {
                    Some(i) => i,
                    None => {
                        self.sets.push(set.clone());
                        let i = self.sets.len().saturating_sub(1);
                        if let Some(s) = self.set_of.get_mut(id) {
                            *s = Some(i);
                        }
                        i
                    }
                };
                vec![I(Inst::Set(i))]
            }
            Node::Any { newline } => vec![I(if *newline { Inst::Any } else { Inst::AnyNoNl })],
            Node::LineBreak => vec![I(Inst::LineBreak)],
            Node::Assert(look) => vec![I(Inst::Assert(*look))],
            Node::Keep => vec![I(Inst::Keep)],
            Node::Concat(items) => items.iter().map(|&i| N(i)).collect(),
            Node::Alt(alts) => {
                let end = self.label();
                let mut seq = Vec::new();
                let last = alts.len().saturating_sub(1);
                for (k, &alt) in alts.iter().enumerate() {
                    if k < last {
                        let (here, next) = (self.label(), self.label());
                        seq.extend([I(Inst::Split(here, next)), L(here), N(alt), I(Inst::Jmp(end)), L(next)]);
                    } else {
                        seq.push(N(alt));
                    }
                }
                seq.push(L(end));
                seq
            }
            Node::Group { index: Some(g), body } => vec![I(Inst::Open(*g)), N(*body), I(Inst::Close(*g))],
            Node::Group { index: None, body } => vec![N(*body)],
            Node::Repeat { body, min, max, kind } => {
                let body = *body;
                let mut seq = Vec::new();
                let lazy = *kind == RepeatKind::Lazy;
                let split = |next, end| if lazy { Inst::Split(end, next) } else { Inst::Split(next, end) };
                if *kind == RepeatKind::Possessive {
                    seq.push(I(Inst::Barrier { fail: None, behind: None }));
                }
                if *max == Some(0) {
                    // Never runs, but the body's groups still need code for calls.
                    let end = self.label();
                    seq.extend([I(Inst::Jmp(end)), N(body), L(end)]);
                }
                // More copies than the cap either overflow it or are empty and change nothing.
                let cap = self.max_insts.saturating_add(1);
                let copies = |n: u32| (0..n).take(cap);
                seq.extend(copies(*min).map(|_| N(body)));
                match max {
                    Some(0) => {}
                    Some(m) => {
                        let end = self.label();
                        for _ in copies(m.saturating_sub(*min)) {
                            let next = self.label();
                            seq.extend([I(split(next, end)), L(next), N(body)]);
                        }
                        seq.push(L(end));
                    }
                    None => {
                        let (top, next, end) = (self.label(), self.label(), self.label());
                        seq.extend([L(top), I(split(next, end)), L(next)]);
                        if self.infos.get(body).is_none_or(|b| b.min == 0) {
                            let mark = self.marks;
                            self.marks = self.marks.saturating_add(1);
                            seq.extend([I(Inst::MarkSet(mark)), N(body), I(Inst::LoopBack { mark, to: top })]);
                        } else {
                            seq.extend([N(body), I(Inst::Jmp(top))]);
                        }
                        seq.push(L(end));
                    }
                }
                if *kind == RepeatKind::Possessive {
                    seq.push(I(Inst::Cut { restore: false, behind: false }));
                }
                seq
            }
            Node::Atomic(body) => {
                vec![I(Inst::Barrier { fail: None, behind: None }), N(*body), I(Inst::Cut { restore: false, behind: false })]
            }
            Node::Lookaround { behind, negate, body } => {
                let end = self.label();
                let mut seq = self.look(*behind, *negate, *body, negate.then_some(end), None);
                seq.push(L(end));
                seq
            }
            Node::Backref { ci, .. } => vec![I(Inst::Backref { group, ci: *ci })],
            Node::Call(_) => vec![I(Inst::Call(group))],
            Node::Cond { cond, yes, no } => {
                let (yes_l, no_l, end) = (self.label(), self.label(), self.label());
                let mut seq = match cond {
                    Cond::Group(_) => vec![I(Inst::CondGroup { group, no: no_l })],
                    Cond::Recursion(g) => vec![I(Inst::CondRec { group: g.as_ref().map(|_| group), no: no_l })],
                    Cond::Define => vec![I(Inst::Jmp(no_l))],
                    Cond::Look(look) => match self.nodes.get(*look) {
                        Some(Node::Lookaround { behind, negate: false, body }) => self.look(*behind, false, *body, Some(no_l), None),
                        Some(Node::Lookaround { behind, negate: true, body }) => self.look(*behind, true, *body, Some(yes_l), Some(no_l)),
                        _ => Vec::new(),
                    },
                };
                seq.extend([L(yes_l), N(*yes), I(Inst::Jmp(end)), L(no_l), N(*no), L(end)]);
                seq
            }
        })
    }

    /// A lookaround: `fail` is where to go if the body fails, `then` where a negative one goes
    /// if the body matches (else it fails).
    fn look(&self, behind: bool, negate: bool, body: NodeId, fail: Option<usize>, then: Option<usize>) -> Vec<Work> {
        let range = behind.then(|| self.infos.get(body).map_or((0, 0), |b| (b.min, b.max.unwrap_or(b.min))));
        let end = if negate { Inst::NegEnd { then, behind } } else { Inst::Cut { restore: true, behind } };
        vec![Work::Inst(Inst::Barrier { fail, behind: range }), Work::Node(body), Work::Inst(end)]
    }
}
