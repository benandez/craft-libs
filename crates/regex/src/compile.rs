//! [`Ast`] → [`Program`]. The arena lists children before parents, so one pass in order builds
//! each node's code from its children's: no recursion. Code fragments use jump targets relative
//! to their own start and are shifted when copied into the parent.

use regex_syntax::hir::ClassUnicode;

use crate::Error;
use crate::ast::{Ast, Cond, GroupRef, Look, Node, RepeatKind};
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
    /// End of a negative lookaround: the body matched, so undo it, then jump to `then` (or fail).
    NegEnd {
        then: Option<usize>,
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
    fn shifted(self, off: usize) -> Inst {
        let s = |pc: usize| pc.saturating_add(off);
        match self {
            Inst::Split(a, b) => Inst::Split(s(a), s(b)),
            Inst::Jmp(a) => Inst::Jmp(s(a)),
            Inst::CondGroup { group, no } => Inst::CondGroup { group, no: s(no) },
            Inst::CondRec { group, no } => Inst::CondRec { group, no: s(no) },
            Inst::Barrier { fail, behind } => Inst::Barrier { fail: fail.map(s), behind },
            Inst::NegEnd { then } => Inst::NegEnd { then: then.map(s) },
            Inst::LoopBack { mark, to } => Inst::LoopBack { mark, to: s(to) },
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
}

fn fixed(n: usize, first: Option<ClassUnicode>) -> Info {
    Info { min: n, max: Some(n), first }
}

fn union(a: Option<ClassUnicode>, b: &Option<ClassUnicode>) -> Option<ClassUnicode> {
    let (mut a, b) = (a?, b.as_ref()?);
    a.union(b);
    Some(a)
}

struct Compiler {
    sets: Vec<CharSet>,
    marks: usize,
    max_insts: usize,
}

fn append(dst: &mut Vec<Inst>, frag: &[Inst], max: usize) -> Result<(), Error> {
    if dst.len().saturating_add(frag.len()) > max {
        return Err(Error::TooComplex);
    }
    let off = dst.len();
    dst.extend(frag.iter().map(|i| i.shifted(off)));
    Ok(())
}

pub(crate) fn compile(ast: Ast, max_insts: usize) -> Result<Program, Error> {
    let Ast { nodes, groups, names, pos } = ast;
    let resolve = |r: &GroupRef, node: usize| -> Result<usize, Error> {
        let at = || pos.get(node).copied().unwrap_or(0);
        let g = match r {
            GroupRef::Index(g) => Some(*g),
            GroupRef::Name { name, pos } => {
                let found = names.iter().find(|(n, _)| **n == **name).map(|(_, g)| *g);
                if found.is_none() {
                    return Err(Error::Syntax { pos: *pos, msg: format!("reference to non-existent group name '{name}'") });
                }
                found
            }
        };
        g.filter(|g| *g < groups).ok_or_else(|| Error::Syntax { pos: at(), msg: "reference to non-existent group".to_owned() })
    };
    let mut c = Compiler { sets: Vec::new(), marks: 0, max_insts };
    let mut frags: Vec<Vec<Inst>> = Vec::with_capacity(nodes.len());
    let mut infos: Vec<Info> = Vec::with_capacity(nodes.len());
    for (id, node) in nodes.iter().enumerate() {
        let mut out = Vec::new();
        let info = c.node(id, node, &mut frags, &infos, &mut out, &resolve, &pos)?;
        frags.push(out);
        infos.push(info);
    }
    let root = frags.pop().unwrap_or_default();
    let root_info = infos.pop().unwrap_or_else(|| fixed(0, None));
    let mut insts = vec![Inst::Open(0)];
    append(&mut insts, &root, max_insts)?;
    append(&mut insts, &[Inst::Close(0), Inst::Match], max_insts)?;
    let mut starts = vec![None; groups];
    for (pc, inst) in insts.iter().enumerate() {
        if let Inst::Open(g) = *inst
            && let Some(s @ None) = starts.get_mut(g)
        {
            *s = Some(pc);
        }
    }
    let first = root_info.first.filter(|_| root_info.min > 0).map(|f| CharSet::new(&f));
    Ok(Program { insts, sets: c.sets, starts, groups, marks: c.marks, first })
}

impl Compiler {
    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        id: usize,
        node: &Node,
        frags: &mut [Vec<Inst>],
        infos: &[Info],
        out: &mut Vec<Inst>,
        resolve: &dyn Fn(&GroupRef, usize) -> Result<usize, Error>,
        pos: &[usize],
    ) -> Result<Info, Error> {
        let max = self.max_insts;
        let take = |frags: &mut [Vec<Inst>], i: usize| frags.get_mut(i).map(std::mem::take).unwrap_or_default();
        let info = |i: usize| infos.get(i).cloned().unwrap_or_else(|| fixed(0, None));
        let unknown = Info { min: 0, max: None, first: None };
        Ok(match node {
            Node::Empty => fixed(0, Some(ClassUnicode::empty())),
            Node::Char(ch) => {
                out.push(Inst::Char(*ch));
                fixed(1, Some(class::of_char(*ch)))
            }
            Node::Set(set) => {
                out.push(Inst::Set(self.sets.len()));
                self.sets.push(set.clone());
                fixed(1, Some(set.class()))
            }
            Node::Any { newline } => {
                out.push(if *newline { Inst::Any } else { Inst::AnyNoNl });
                fixed(1, None)
            }
            Node::LineBreak => {
                out.push(Inst::LineBreak);
                let v = class::perl('v');
                Info { min: 1, max: Some(2), first: v }
            }
            Node::Assert(look) => {
                out.push(Inst::Assert(*look));
                fixed(0, Some(ClassUnicode::empty()))
            }
            Node::Keep => {
                out.push(Inst::Keep);
                fixed(0, Some(ClassUnicode::empty()))
            }
            Node::Concat(items) => {
                let mut acc = Info { min: 0, max: Some(0), first: Some(ClassUnicode::empty()) };
                let mut open = true;
                for &i in items {
                    append(out, &take(frags, i), max)?;
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
                let mut acc: Option<Info> = None;
                let mut jumps = Vec::new();
                let last = alts.len().saturating_sub(1);
                for (k, &i) in alts.iter().enumerate() {
                    let body = take(frags, i);
                    if k < last {
                        let next = out.len().saturating_add(body.len()).saturating_add(2);
                        out.push(Inst::Split(out.len().saturating_add(1), next));
                        append(out, &body, max)?;
                        jumps.push(out.len());
                        out.push(Inst::Jmp(0));
                    } else {
                        append(out, &body, max)?;
                    }
                    let it = info(i);
                    acc = Some(match acc {
                        None => it,
                        Some(a) => Info { min: a.min.min(it.min), max: a.max.zip(it.max).map(|(a, b)| a.max(b)), first: union(a.first, &it.first) },
                    });
                }
                let end = out.len();
                for j in jumps {
                    if let Some(inst) = out.get_mut(j) {
                        *inst = Inst::Jmp(end);
                    }
                }
                acc.unwrap_or_else(|| fixed(0, Some(ClassUnicode::empty())))
            }
            Node::Group { index, body } => {
                if let Some(g) = index {
                    out.push(Inst::Open(*g));
                }
                append(out, &take(frags, *body), max)?;
                if let Some(g) = index {
                    out.push(Inst::Close(*g));
                }
                info(*body)
            }
            Node::Repeat { body, min, max: rmax, kind } => {
                let b = take(frags, *body);
                let bi = info(*body);
                let (n, m) = (usize::try_from(*min).unwrap_or(usize::MAX), rmax.map(|m| usize::try_from(m).unwrap_or(usize::MAX)));
                let possessive = *kind == RepeatKind::Possessive;
                let lazy = *kind == RepeatKind::Lazy;
                if possessive {
                    out.push(Inst::Barrier { fail: None, behind: None });
                }
                if m == Some(0) {
                    // Never runs, but the body's groups still need code for calls.
                    out.push(Inst::Jmp(out.len().saturating_add(b.len()).saturating_add(1)));
                    append(out, &b, max)?;
                }
                for _ in 0..n {
                    append(out, &b, max)?;
                }
                match m {
                    Some(0) => {}
                    Some(m) => {
                        let opt = m.saturating_sub(n);
                        let end = out.len().saturating_add(opt.saturating_mul(b.len().saturating_add(1)));
                        for _ in 0..opt {
                            let next = out.len().saturating_add(1);
                            out.push(if lazy { Inst::Split(end, next) } else { Inst::Split(next, end) });
                            append(out, &b, max)?;
                        }
                    }
                    None => {
                        let top = out.len();
                        let mark = (bi.min == 0).then(|| {
                            self.marks = self.marks.saturating_add(1);
                            self.marks.saturating_sub(1)
                        });
                        let len = b.len().saturating_add(2).saturating_add(usize::from(mark.is_some()));
                        let (next, end) = (top.saturating_add(1), top.saturating_add(len));
                        out.push(if lazy { Inst::Split(end, next) } else { Inst::Split(next, end) });
                        if let Some(mark) = mark {
                            out.push(Inst::MarkSet(mark));
                        }
                        append(out, &b, max)?;
                        out.push(match mark {
                            Some(mark) => Inst::LoopBack { mark, to: top },
                            None => Inst::Jmp(top),
                        });
                    }
                }
                if possessive {
                    out.push(Inst::Cut { restore: false, behind: false });
                }
                if out.len() > max {
                    return Err(Error::TooComplex);
                }
                Info {
                    min: bi.min.saturating_mul(n),
                    max: m.and_then(|m| bi.max.and_then(|x| x.checked_mul(m))),
                    first: if m == Some(0) { Some(ClassUnicode::empty()) } else { bi.first },
                }
            }
            Node::Atomic(body) => {
                out.push(Inst::Barrier { fail: None, behind: None });
                append(out, &take(frags, *body), max)?;
                out.push(Inst::Cut { restore: false, behind: false });
                info(*body)
            }
            Node::Lookaround { behind, negate, body } => {
                let bi = info(*body);
                let range = match (behind, bi.max) {
                    (false, _) => None,
                    (true, Some(m)) if m <= MAX_LOOKBEHIND => Some((bi.min, m)),
                    (true, _) => {
                        let at = pos.get(id).copied().unwrap_or(0);
                        return Err(Error::Syntax { pos: at, msg: "lookbehind must have a bounded length".to_owned() });
                    }
                };
                let b = take(frags, *body);
                let end = b.len().saturating_add(2);
                out.push(Inst::Barrier { fail: negate.then_some(end), behind: range });
                append(out, &b, max)?;
                out.push(if *negate { Inst::NegEnd { then: None } } else { Inst::Cut { restore: true, behind: *behind } });
                fixed(0, Some(ClassUnicode::empty()))
            }
            Node::Backref { group, ci } => {
                let g = resolve(group, id)?;
                if g == 0 {
                    let at = pos.get(id).copied().unwrap_or(0);
                    return Err(Error::Syntax { pos: at, msg: "a backreference cannot refer to group 0".to_owned() });
                }
                out.push(Inst::Backref { group: g, ci: *ci });
                unknown
            }
            Node::Call(group) => {
                out.push(Inst::Call(resolve(group, id)?));
                unknown
            }
            Node::Cond { cond, yes, no } => {
                let (y, n) = (take(frags, *yes), take(frags, *no));
                match cond {
                    Cond::Group(g) => {
                        let g = resolve(g, id)?;
                        out.push(Inst::CondGroup { group: g, no: y.len().saturating_add(2) });
                    }
                    Cond::Recursion(g) => {
                        let g = g.as_ref().map(|g| resolve(g, id)).transpose()?;
                        out.push(Inst::CondRec { group: g, no: y.len().saturating_add(2) });
                    }
                    Cond::Define => out.push(Inst::Jmp(y.len().saturating_add(2))),
                    Cond::Look(look) => {
                        let mut l = take(frags, *look);
                        let no = l.len().saturating_add(y.len()).saturating_add(1);
                        match l.last_mut() {
                            Some(Inst::NegEnd { then }) => *then = Some(no),
                            _ => {
                                if let Some(Inst::Barrier { fail, .. }) = l.first_mut() {
                                    *fail = Some(no);
                                }
                            }
                        }
                        append(out, &l, max)?;
                    }
                }
                append(out, &y, max)?;
                let end = out.len().saturating_add(n.len()).saturating_add(1);
                out.push(Inst::Jmp(end));
                append(out, &n, max)?;
                let (yi, ni) = (info(*yes), info(*no));
                Info { min: yi.min.min(ni.min), max: yi.max.zip(ni.max).map(|(a, b)| a.max(b)), first: union(yi.first, &ni.first) }
            }
        })
    }
}
