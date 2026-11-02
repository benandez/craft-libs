//! Flag-based compounding, as the hunspell(5) man page describes it and the `hunspell` 1.7.5 CLI
//! answers on synthetic dictionaries (black-box; every rule below has a test pinned to the CLI's
//! answer in the tests below).
//!
//! A compound is two or more parts, each at least `COMPOUNDMIN` characters, each a stem or an
//! affixed form of one whose flags (the stem's, and the continuation classes of its affixes)
//! allow its place: `COMPOUNDFLAG` anywhere, `COMPOUNDBEGIN` first, `COMPOUNDMIDDLE` inside,
//! `COMPOUNDEND` last. (`COMPOUNDFIRST` and `COMPOUNDLAST` are ignored, as the CLI does.)
//!
//! - A prefix may sit on any part but the last and a suffix only on the last, unless the affix
//!   carries `COMPOUNDPERMITFLAG`. Two suffixes in a row (continuation classes) only on the last.
//! - An affix carrying `ONLYINCOMPOUND` is only valid inside a compound: a prefix anywhere, a
//!   suffix anywhere but on the last part (the CLI's answers; the word alone is no word).
//! - `COMPOUNDFORBIDFLAG` on a stem or an affix: that part may only be last.
//! - A stem with `NEEDAFFIX` is a part only with an affix; `FORBIDDENWORD` stems are no part, and
//!   a forbidden word is never rebuilt from parts.
//! - `COMPOUNDWORDMAX`: most parts (a `COMPOUNDROOT` stem counts as two). With `COMPOUNDSYLLABLE`
//!   and `LANG hu`, more parts are allowed while the word has at most that many vowels; with
//!   `COMPOUNDSYLLABLE` and another language the CLI applies no limit, and neither does this.
//! - `CHECKCOMPOUNDDUP`: the last two parts may not be the same stem (`foofoo`; `foofoobar` is
//!   fine, as in the CLI).
//! - `CHECKCOMPOUNDCASE`: no capital on either side of a boundary, unless the other side is `-`.
//! - `CHECKCOMPOUNDTRIPLE`: no three equal letters across a boundary (`schiff|fahrt`).
//!   `SIMPLIFIEDTRIPLE`: a part ending in a double letter may share its last letter with the next
//!   part (`schiffahrt` = `schiff` + `fahrt`).
//! - `CHECKCOMPOUNDPATTERN end[/flag] begin[/flag]`: no boundary where the part before ends with
//!   `end` and the part after begins with `begin` (and they carry the flags); `0` for `end` means
//!   an unaffixed part. The optional replacement field is refused.
//! - `CHECKCOMPOUNDREP`: no compound that a `REP` replacement turns into a word (not a compound),
//!   checked on every two neighbouring parts and on every run of parts that ends the word.
//! - A compound of three or more parts is rejected when one slip turns it into a word (not a
//!   compound): see [`AffixDict::near_word`]. The CLI's answers, with or without
//!   `CHECKCOMPOUNDREP`.
//! - `FORCEUCASE` on the last part: only the capitalized word (or the word in capitals) is right.
//! - `MAXCPDSUGS 0`: compounds are never suggested.
//!
//! Not read: `SYLLABLENUM` (Hungarian), which the man page doesn't describe; it is ignored.
//!
//! The work per word is bounded: words up to [`MAX_WORD_BYTES`](super::MAX_WORD_BYTES), at most
//! [`MAX_PARTS`] parts, each part's analyses computed once, and at most [`MAX_WORK`] steps (rules
//! tried and splits followed) per word; past that the word is no compound.

use super::{AffixDict, Excluded, FlagKey, FlagMode, Rule, has};

/// Most parts a compound may have, whatever `COMPOUNDWORDMAX` says.
pub(super) const MAX_PARTS: usize = 16;
/// Most steps one compound check may take (affix rules tried, parts looked up, splits followed).
pub(super) const MAX_WORK: usize = 1 << 16;
/// Most `CHECKCOMPOUNDPATTERN` lines (the Dutch dictionary has 52).
const MAX_PATTERNS: usize = 512;
/// Longest text on either side of a `CHECKCOMPOUNDPATTERN`, in bytes.
const MAX_PATTERN_BYTES: usize = 64;
/// Most vowels a `COMPOUNDSYLLABLE` line may list.
const MAX_VOWELS: usize = 64;
/// Most `MAP` variants, `TRY` letters and `KEY` row letters the three-part check tries.
const MAX_MAP_VARIANTS: usize = 256;
const MAX_TRY: usize = 128;
const MAX_KEY_ROW: usize = 128;
/// The keyboard rows the CLI assumes when a dictionary has no `KEY`.
const DEFAULT_KEY: &str = "qwertyuiop|asdfghjkl|zxcvbnm";

/// The part may come first, in the middle, last.
const FIRST: u8 = 1;
const MIDDLE: u8 = 2;
const LAST: u8 = 4;

/// The single-flag compounding directives, in the order [`Raw::flags`] keeps them.
const FLAG_DIRECTIVES: [&str; 8] =
    ["COMPOUNDFLAG", "COMPOUNDBEGIN", "COMPOUNDMIDDLE", "COMPOUNDEND", "COMPOUNDPERMITFLAG", "COMPOUNDFORBIDFLAG", "COMPOUNDROOT", "FORCEUCASE"];
const FLAG: usize = 0;
const BEGIN: usize = 1;
const MIDDLE_FLAG: usize = 2;
const END: usize = 3;
const PERMIT: usize = 4;
const FORBID: usize = 5;
const ROOT: usize = 6;
const FORCE_UCASE: usize = 7;

/// The yes/no compounding directives, in the order [`Raw::switches`] keeps them.
const SWITCHES: [&str; 5] = ["CHECKCOMPOUNDDUP", "CHECKCOMPOUNDREP", "CHECKCOMPOUNDCASE", "CHECKCOMPOUNDTRIPLE", "SIMPLIFIEDTRIPLE"];
const DUP: usize = 0;
const REP: usize = 1;
const CASE: usize = 2;
const TRIPLE: usize = 3;
const SIMPLIFIED: usize = 4;

/// Compounding directives as read from the `.aff` or the compact format, before flags get ids.
#[derive(Clone, Debug, Default)]
pub(super) struct Raw {
    flags: [Option<FlagKey>; 8],
    switches: [bool; 5],
    word_max: Option<usize>,
    /// `COMPOUNDSYLLABLE`: most vowels, and the vowels.
    syllable: Option<(usize, String)>,
    /// `LANG hu…`: the Hungarian syllable rule applies.
    hungarian: bool,
    /// `CHECKCOMPOUNDPATTERN` lines, as written (`end[/flag] begin[/flag]`).
    patterns: Vec<(String, String)>,
    pattern_header: bool,
    /// `MAXCPDSUGS 0`.
    no_suggestions: bool,
    /// `NOSPLITSUGS`.
    no_split_suggestions: bool,
    /// `COMPOUNDMORESUFFIXES`: refused with flag-based compounding, which it would change;
    /// ignored without (Korean has it beside `COMPOUNDRULE`s only).
    more_suffixes: bool,
}

impl Raw {
    /// Read the compounding directive `dir` of the line `f` (its fields); `Ok(false)` when `dir`
    /// is none. `one_flag` reads a flag in the dictionary's `FLAG` mode.
    pub(super) fn read(&mut self, f: &[&str], one_flag: &mut impl FnMut(&str) -> Result<FlagKey, String>) -> Result<bool, String> {
        let Some(&dir) = f.first() else { return Ok(false) };
        let at = |k: usize| f.get(k).copied().unwrap_or("");
        if let Some(i) = FLAG_DIRECTIVES.iter().position(|&d| d == dir) {
            let k = one_flag(at(1)).map_err(|e| format!("{dir} needs one flag: {e}"))?;
            if let Some(slot) = self.flags.get_mut(i) {
                *slot = Some(k);
            }
            return Ok(true);
        }
        if let Some(i) = SWITCHES.iter().position(|&d| d == dir) {
            if let Some(s) = self.switches.get_mut(i) {
                *s = true;
            }
            return Ok(true);
        }
        match dir {
            // `COMPOUNDWORDMAX 2 y` (Hungarian): the number is the first field.
            "COMPOUNDWORDMAX" => {
                let n = at(1).parse::<usize>().map_err(|_| "COMPOUNDWORDMAX needs a number".to_string())?;
                self.word_max = Some(n.clamp(1, MAX_PARTS));
            }
            "COMPOUNDSYLLABLE" => {
                let n = at(1).parse::<usize>().map_err(|_| "COMPOUNDSYLLABLE needs a number".to_string())?;
                let vowels: String = at(2).chars().take(MAX_VOWELS).collect();
                self.syllable = Some((n.min(super::MAX_WORD_BYTES), vowels));
            }
            "LANG" => self.hungarian = at(1) == "hu" || at(1).starts_with("hu_") || at(1).starts_with("hu-"),
            "MAXCPDSUGS" => self.no_suggestions = at(1) == "0",
            "NOSPLITSUGS" => self.no_split_suggestions = true,
            "COMPOUNDMORESUFFIXES" => self.more_suffixes = true,
            "CHECKCOMPOUNDPATTERN" => self.read_pattern(f)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// A `CHECKCOMPOUNDPATTERN` line: the count line, or `end[/flag] begin[/flag]` with an
    /// optional comment (`# …`). A replacement field is refused: no Language pack uses one.
    fn read_pattern(&mut self, f: &[&str]) -> Result<(), String> {
        let fields: Vec<&str> = f.iter().skip(1).copied().take_while(|x| !x.starts_with('#')).collect();
        if !self.pattern_header && fields.len() == 1 && fields.first().is_some_and(|x| x.parse::<usize>().is_ok()) {
            self.pattern_header = true;
            return Ok(());
        }
        let [end, begin] = fields.as_slice() else {
            return Err(format!("CHECKCOMPOUNDPATTERN `{}`: only `end[/flag] begin[/flag]` is supported", fields.join(" ")));
        };
        if end.len() > MAX_PATTERN_BYTES || begin.len() > MAX_PATTERN_BYTES || self.patterns.len() >= MAX_PATTERNS {
            return Err(format!("CHECKCOMPOUNDPATTERN: over {MAX_PATTERNS} patterns or {MAX_PATTERN_BYTES} bytes"));
        }
        self.patterns.push(((*end).to_string(), (*begin).to_string()));
        Ok(())
    }

    /// Whether any directive makes words compound by flags (the rest only refine that).
    fn compounds(&self) -> bool {
        [FLAG, BEGIN, MIDDLE_FLAG, END].iter().any(|&i| self.flags.get(i).is_some_and(Option::is_some))
    }

    /// Every flag the directives and patterns name, so that each gets an id.
    pub(super) fn keys(&self, mode: FlagMode) -> Result<Vec<FlagKey>, String> {
        let mut keys: Vec<FlagKey> = self.flags.iter().flatten().copied().collect();
        for (end, begin) in &self.patterns {
            for side in [end, begin] {
                if let Some((_, flag)) = side.split_once('/') {
                    keys.push(mode.parse_one(flag).map_err(|e| format!("CHECKCOMPOUNDPATTERN `{side}`: {e}"))?);
                }
            }
        }
        Ok(keys)
    }

    /// The compounding of a dictionary whose flags got ids by `id_of`; `None` when it doesn't
    /// compound by flags.
    pub(super) fn build(&self, mode: FlagMode, id_of: &impl Fn(FlagKey) -> Option<u16>) -> Result<Option<Box<Compounding>>, String> {
        if !self.compounds() {
            return Ok(None);
        }
        if self.more_suffixes {
            return Err("COMPOUNDMORESUFFIXES is not supported with flag-based compounding".into());
        }
        let id = |i: usize| self.flags.get(i).copied().flatten().and_then(id_of);
        let switch = |i: usize| self.switches.get(i).copied().unwrap_or(false);
        let mut c = Compounding {
            flag: id(FLAG),
            begin: id(BEGIN),
            middle: id(MIDDLE_FLAG),
            end: id(END),
            permit: id(PERMIT),
            forbid: id(FORBID),
            root: id(ROOT),
            force_ucase: id(FORCE_UCASE),
            dup: switch(DUP),
            rep: switch(REP),
            case: switch(CASE),
            triple: switch(TRIPLE),
            simplified: switch(SIMPLIFIED),
            no_suggestions: self.no_suggestions,
            two_words: !self.no_split_suggestions,
            raw: self.clone(),
            ..Compounding::default()
        };
        // `COMPOUNDSYLLABLE` turns `COMPOUNDWORDMAX` into the Hungarian syllable rule, or (other
        // languages) lifts it: the CLI's answers.
        match (&self.syllable, self.hungarian) {
            (None, _) => c.word_max = self.word_max,
            (Some((n, vowels)), true) => {
                c.word_max = self.word_max;
                c.syllables = Some((*n, vowels.chars().collect()));
            }
            (Some(_), false) => {}
        }
        if let Some(f) = c.force_ucase {
            c.marks.push(f);
        }
        for (end, begin) in &self.patterns {
            let side = |text: &str, c: &mut Compounding| -> Result<(String, Need), String> {
                let (chars, flag) = text.split_once('/').map_or((text, None), |(a, b)| (a, Some(b)));
                let need = match flag {
                    None => Need::Any,
                    Some(flag) => match id_of(mode.parse_one(flag)?) {
                        Some(f) => Need::Mark(c.mark(f)?),
                        None => Need::Never,
                    },
                };
                Ok((chars.to_string(), need))
            };
            let (end_chars, end_need) = side(end, &mut c)?;
            let (begin, begin_need) = side(begin, &mut c)?;
            let unmodified = end_chars == "0";
            let end = if unmodified { String::new() } else { end_chars };
            c.patterns.push(Pattern { end: end.into(), end_need, begin: begin.into(), begin_need, unmodified });
        }
        Ok(Some(Box::new(c)))
    }
}

/// What a side of a `CHECKCOMPOUNDPATTERN` asks of a part's flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Any,
    /// The flag of [`Compounding::marks`] at this index.
    Mark(u8),
    /// A flag no stem or rule carries.
    Never,
}

impl Need {
    fn met(self, marks: u64) -> bool {
        match self {
            Need::Any => true,
            Need::Mark(i) => marks & (1 << i) != 0,
            Need::Never => false,
        }
    }
}

#[derive(Clone, Debug)]
struct Pattern {
    end: Box<str>,
    end_need: Need,
    begin: Box<str>,
    begin_need: Need,
    /// `0` for `end`: the part before is a stem without affixes.
    unmodified: bool,
}

/// The flag-based compounding of a dictionary (see the module docs).
#[derive(Clone, Debug, Default)]
pub(super) struct Compounding {
    flag: Option<u16>,
    begin: Option<u16>,
    middle: Option<u16>,
    end: Option<u16>,
    permit: Option<u16>,
    forbid: Option<u16>,
    root: Option<u16>,
    force_ucase: Option<u16>,
    word_max: Option<usize>,
    /// The Hungarian rule: past `word_max` parts, at most this many vowels in the word.
    syllables: Option<(usize, Box<[char]>)>,
    dup: bool,
    rep: bool,
    case: bool,
    triple: bool,
    simplified: bool,
    pub(super) no_suggestions: bool,
    /// A compound of three or more parts that splits into two words is wrong (unless
    /// `NOSPLITSUGS`: the CLI ties the two to each other).
    two_words: bool,
    patterns: Vec<Pattern>,
    /// Flags tracked per part (`FORCEUCASE`, the patterns' flags): bit `i` of [`Info::marks`].
    marks: Vec<u16>,
    /// The directives as read, written back by [`Compounding::write`].
    raw: Raw,
}

impl Compounding {
    /// The mark index of `flag`, adding it.
    fn mark(&mut self, flag: u16) -> Result<u8, String> {
        let i = match self.marks.iter().position(|&m| m == flag) {
            Some(i) => i,
            None => {
                self.marks.push(flag);
                self.marks.len() - 1
            }
        };
        u8::try_from(i).ok().filter(|&i| i < 64).ok_or_else(|| "CHECKCOMPOUNDPATTERN: more than 63 distinct flags".to_string())
    }

    /// The directives as compact-format lines (flags written by `write_flag`).
    pub(super) fn write(&self, out: &mut String, write_flag: &impl Fn(FlagKey, &mut String)) {
        let raw = &self.raw;
        if raw.hungarian {
            out.push_str("LANG hu\n");
        }
        for (name, key) in FLAG_DIRECTIVES.iter().zip(&raw.flags) {
            if let Some(k) = key {
                out.push_str(name);
                out.push(' ');
                write_flag(*k, out);
                out.push('\n');
            }
        }
        for (name, on) in SWITCHES.iter().zip(&raw.switches) {
            if *on {
                out.push_str(name);
                out.push('\n');
            }
        }
        if let Some(n) = raw.word_max {
            out.push_str(&format!("COMPOUNDWORDMAX {n}\n"));
        }
        if let Some((n, vowels)) = &raw.syllable {
            out.push_str(&format!("COMPOUNDSYLLABLE {n} {vowels}\n"));
        }
        if raw.no_suggestions {
            out.push_str("MAXCPDSUGS 0\n");
        }
        if raw.no_split_suggestions {
            out.push_str("NOSPLITSUGS\n");
        }
        for (end, begin) in &raw.patterns {
            out.push_str(&format!("CHECKCOMPOUNDPATTERN {end} {begin}\n"));
        }
    }
}

/// One way a piece of the word can be a compound part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Info {
    /// Where it may sit: [`FIRST`], [`MIDDLE`], [`LAST`].
    pos: u8,
    /// Which of [`Compounding::marks`] its flags hold.
    marks: u64,
    /// No affix.
    unmodified: bool,
    /// The stem's index (`CHECKCOMPOUNDDUP` compares stems).
    stem: u32,
    /// How many words it counts as (`COMPOUNDROOT`: two).
    words: u8,
}

/// A stem found for a part, and the affixes that make the part from it.
#[derive(Clone, Copy)]
struct Analysis<'a> {
    stem: u32,
    set: &'a [u16],
    pfx: Option<&'a Rule>,
    sfx: Option<&'a Rule>,
    /// The inner suffix of two in a row (`sfx` is the outer one).
    inner: Option<&'a Rule>,
}

/// One compound check: the word, its parts' analyses (each piece looked up once), the parts
/// chosen so far and the work done.
struct Search<'a> {
    c: &'a Compounding,
    w: &'a str,
    /// Byte offset of each char, and `w.len()`.
    cuts: Vec<usize>,
    chars: Vec<char>,
    excluded: Excluded,
    capitalized: bool,
    /// Per piece `(start, end)` in chars: its infos as a range of `infos`.
    memo: Vec<Option<(u32, u32)>>,
    infos: Vec<Info>,
    /// The parts so far: start and end char, and the info.
    path: Vec<(usize, usize, Info)>,
    work: usize,
    /// [`AffixDict::near_word`], once answered.
    near: Option<bool>,
}

impl AffixDict {
    /// Is `w` a compound by the flag-based rules (see the module docs)? `capitalized`: the word
    /// as the user wrote it starts with a capital (`FORCEUCASE`).
    pub(super) fn flag_compound(&self, w: &str, excluded: Excluded, capitalized: bool) -> bool {
        let Some(c) = self.cpd.as_deref() else { return false };
        if excluded.no_compounds() || w.is_empty() || w.len() > super::MAX_WORD_BYTES {
            return false;
        }
        if self.is_forbidden(w) {
            return false;
        }
        let cuts: Vec<usize> = w.char_indices().map(|(i, _)| i).chain(std::iter::once(w.len())).collect();
        let chars: Vec<char> = w.chars().collect();
        let n = cuts.len();
        let mut s = Search {
            c,
            w,
            cuts,
            chars,
            excluded,
            capitalized,
            memo: vec![None; n * n],
            infos: Vec::new(),
            path: Vec::with_capacity(MAX_PARTS),
            work: 0,
            near: None,
        };
        self.parts_from(&mut s, 0)
    }

    /// Can the rest of the word, from char `start`, be split into parts after `s.path`?
    fn parts_from(&self, s: &mut Search, start: usize) -> bool {
        let total = s.chars.len();
        let count = s.path.len();
        let min = self.compound_min.max(1);
        for end in start.saturating_add(min)..=total {
            s.work += 1;
            if s.work > MAX_WORK {
                return false;
            }
            let last = end == total;
            if !last && count + 1 >= MAX_PARTS {
                break;
            }
            let place = if last {
                LAST
            } else if count == 0 {
                FIRST
            } else {
                MIDDLE
            };
            let (a, b) = self.infos_of(s, start, end);
            for k in a..b {
                let Some(info) = s.infos.get(k as usize).copied() else { continue };
                if info.pos & place == 0 || !self.boundary_ok(s, start, &info) {
                    continue;
                }
                s.path.push((start, end, info));
                let ok = if last { self.whole_ok(s) } else { self.words_ok(s, false) && self.next_parts(s, end) };
                s.path.pop();
                if ok {
                    return true;
                }
            }
        }
        false
    }

    /// After a part ending at char `end`: the next part starts there, or (`SIMPLIFIEDTRIPLE`)
    /// one char earlier when the part ends in a double letter.
    fn next_parts(&self, s: &mut Search, end: usize) -> bool {
        if self.parts_from(s, end) {
            return true;
        }
        let double = end >= 2 && s.chars.get(end - 1).is_some_and(|c| s.chars.get(end - 2) == Some(c));
        s.c.simplified && double && self.parts_from(s, end - 1)
    }

    /// The analyses of the piece from char `a` to char `b`, computed once: a range of `s.infos`.
    fn infos_of(&self, s: &mut Search, a: usize, b: usize) -> (u32, u32) {
        let n = s.cuts.len();
        let slot = a.saturating_mul(n).saturating_add(b);
        if let Some(Some(r)) = s.memo.get(slot) {
            return *r;
        }
        let from = u32::try_from(s.infos.len()).unwrap_or(u32::MAX);
        if let (Some(&x), Some(&y)) = (s.cuts.get(a), s.cuts.get(b))
            && let Some(piece) = s.w.get(x..y)
        {
            let mut found: Vec<Info> = Vec::new();
            let mut work = s.work;
            self.analyses(piece, s.excluded, &mut work, &mut |an: Analysis| {
                if let Some(info) = self.info(s.c, &an)
                    && !found.contains(&info)
                {
                    found.push(info);
                }
            });
            s.work = work;
            s.infos.extend(found);
        }
        let to = u32::try_from(s.infos.len()).unwrap_or(u32::MAX);
        if let Some(m) = s.memo.get_mut(slot) {
            *m = Some((from, to));
        }
        (from, to)
    }

    /// Where the analysis `an` lets its piece sit in a compound, if anywhere.
    fn info(&self, c: &Compounding, an: &Analysis) -> Option<Info> {
        let sets = [Some(an.set), an.pfx.map(|r| self.set(r.cont)), an.sfx.map(|r| self.set(r.cont)), an.inner.map(|r| self.set(r.cont))];
        let carries = |f: Option<u16>| f.is_some_and(|f| sets.iter().flatten().any(|s| has(s, f)));
        let mut pos = 0;
        if carries(c.flag) {
            pos = FIRST | MIDDLE | LAST;
        }
        for (flag, p) in [(c.begin, FIRST), (c.middle, MIDDLE), (c.end, LAST)] {
            if carries(flag) {
                pos |= p;
            }
        }
        // A prefix without COMPOUNDPERMITFLAG: any part but the last (`foo|unfoo|bar` is right
        // in the CLI, `foo|unfoo` wrong).
        if an.pfx.is_some_and(|r| !self.cont_holds(r, c.permit)) {
            pos &= !LAST;
        }
        if let Some(r) = an.sfx {
            if !self.cont_holds(r, c.permit) {
                pos &= LAST;
            }
            if self.cont_holds(r, self.only_in_compound) {
                pos &= !LAST;
            }
        }
        if an.inner.is_some() {
            pos &= LAST;
        }
        if carries(c.forbid) {
            pos &= LAST;
        }
        if pos == 0 {
            return None;
        }
        let mut marks = 0u64;
        for (i, &m) in c.marks.iter().enumerate().take(64) {
            if carries(Some(m)) {
                marks |= 1 << i;
            }
        }
        let words = if carries(c.root) { 2 } else { 1 };
        Some(Info { pos, marks, unmodified: an.pfx.is_none() && an.sfx.is_none(), stem: an.stem, words })
    }

    /// Every way `p` is a stem, or a stem with a prefix, one or two suffixes, or a prefix and a
    /// suffix, as a compound part may be (`ONLYINCOMPOUND` affixes and stems included).
    fn analyses<'a>(&'a self, p: &str, excluded: Excluded, work: &mut usize, emit: &mut impl FnMut(Analysis<'a>)) {
        if let Some((stem, set)) = self.part_stem(p, excluded)
            && !self.need_affix.is_some_and(|n| has(set, n))
        {
            emit(Analysis { stem, set, pfx: None, sfx: None, inner: None });
        }
        self.suffix_analyses(p, None, excluded, work, emit);
        let mut root = String::with_capacity(p.len() + 8);
        let ends = std::iter::once(0).chain(p.char_indices().map(|(i, c)| i + c.len_utf8()));
        for b in ends.take(self.max_pfx_chars + 1) {
            let (Some(add), Some(rest)) = (p.get(..b), p.get(b..)) else { continue };
            if rest.is_empty() && !self.full_strip {
                continue;
            }
            let Some(ids) = self.pfx_index.get(add) else { continue };
            for r in ids.iter().filter_map(|&i| self.prefixes.get(i as usize)) {
                *work += 1;
                if *work > MAX_WORK {
                    return;
                }
                root.clear();
                root.push_str(&r.strip);
                root.push_str(rest);
                if !r.cond_matches(&root, false) {
                    continue;
                }
                let (circ, needs) = (self.cont_holds(r, self.circumfix), self.cont_holds(r, self.need_affix));
                if !circ
                    && !needs
                    && let Some((stem, set)) = self.part_stem(&root, excluded)
                    && has(set, r.flag)
                {
                    emit(Analysis { stem, set, pfx: Some(r), sfx: None, inner: None });
                }
                if r.cross {
                    self.suffix_analyses(&root, Some(r), excluded, work, emit);
                }
            }
        }
    }

    /// The analyses of `p` that end in a suffix: on a stem (after the prefix `pfx`, if any), or
    /// (without a prefix) on a form of another suffix.
    fn suffix_analyses<'a>(&'a self, p: &str, pfx: Option<&'a Rule>, excluded: Excluded, work: &mut usize, emit: &mut impl FnMut(Analysis<'a>)) {
        let mut root = String::with_capacity(p.len() + 8);
        let cuts = std::iter::once(p.len()).chain(p.char_indices().rev().map(|(i, _)| i).filter(|&i| i > 0 || self.full_strip));
        for b in cuts.take(self.max_sfx_chars + 1) {
            let (Some(base), Some(add)) = (p.get(..b), p.get(b..)) else { continue };
            let Some(groups) = self.sfx_index.get(add) else { continue };
            for g in groups {
                root.clear();
                root.push_str(base);
                root.push_str(&g.strip);
                let rules = || g.rules.iter().filter_map(|&i| self.suffixes.get(i as usize));
                if let Some((stem, set)) = self.part_stem(&root, excluded) {
                    for r in rules() {
                        *work += 1;
                        if *work > MAX_WORK {
                            return;
                        }
                        if self.suffix_fits(r, set, pfx) && r.cond_matches(&root, true) {
                            emit(Analysis { stem, set, pfx, sfx: Some(r), inner: None });
                        }
                    }
                }
                // Two suffixes in a row: the outer ones that another suffix's continuation
                // classes can license, looked for together under one inner scan.
                if pfx.is_none() && !g.outer_rules.is_empty() {
                    let outers: Vec<&Rule> = g
                        .outer_rules
                        .iter()
                        .filter_map(|&i| self.suffixes.get(i as usize))
                        .filter(|r| !self.cont_holds(r, self.only_in_compound) && r.cond_matches(&root, true))
                        .collect();
                    *work += outers.len();
                    if !outers.is_empty() {
                        self.inner_analyses(&root, &outers, excluded, work, emit);
                    }
                }
            }
        }
    }

    /// Does the suffix `r` apply to a stem with the flags `set`, after the prefix `pfx` (if any)?
    /// Circumfix alike; a suffix that needs another affix only after a prefix that doesn't.
    fn suffix_fits(&self, r: &Rule, set: &[u16], pfx: Option<&Rule>) -> bool {
        if !has(set, r.flag) {
            return false;
        }
        let circ = self.cont_holds(r, self.circumfix);
        let needs = self.cont_holds(r, self.need_affix);
        match pfx {
            None => !circ && !needs,
            Some(p) => {
                r.cross
                    && (has(set, p.flag) || self.cont_holds(r, Some(p.flag)))
                    && circ == self.cont_holds(p, self.circumfix)
                    && !(needs && self.cont_holds(p, self.need_affix))
            }
        }
    }

    /// `form` (what an outer suffix of `outers` was taken off) as a stem with an inner suffix
    /// whose continuation classes hold that outer suffix's flag.
    fn inner_analyses<'a>(&'a self, form: &str, outers: &[&'a Rule], excluded: Excluded, work: &mut usize, emit: &mut impl FnMut(Analysis<'a>)) {
        let mut root = String::with_capacity(form.len() + 8);
        let cuts = std::iter::once(form.len()).chain(form.char_indices().rev().map(|(i, _)| i).filter(|&i| i > 0 || self.full_strip));
        for b in cuts.take(self.max_sfx_chars + 1) {
            let (Some(base), Some(add)) = (form.get(..b), form.get(b..)) else { continue };
            let Some(groups) = self.sfx_index.get(add) else { continue };
            for g in groups.iter().filter(|g| !g.cont_rules.is_empty()) {
                root.clear();
                root.push_str(base);
                root.push_str(&g.strip);
                let Some((stem, set)) = self.part_stem(&root, excluded) else { continue };
                for r in g.cont_rules.iter().filter_map(|&i| self.suffixes.get(i as usize)) {
                    *work += 1;
                    if *work > MAX_WORK {
                        return;
                    }
                    if !has(set, r.flag)
                        || self.cont_holds(r, self.circumfix)
                        || self.cont_holds(r, self.only_in_compound)
                        || !r.cond_matches(&root, true)
                    {
                        continue;
                    }
                    for &outer in outers.iter().filter(|o| self.cont_holds(r, Some(o.flag))) {
                        *work += 1;
                        emit(Analysis { stem, set, pfx: None, sfx: Some(outer), inner: Some(r) });
                    }
                }
            }
        }
    }

    /// The stem `w` as a compound part: its index and flags, unless it is forbidden or excluded.
    fn part_stem(&self, w: &str, excluded: Excluded) -> Option<(u32, &[u16])> {
        self.stem_lookup(w).filter(|(_, set)| !excluded.hits(set) && !self.forbidden.is_some_and(|f| has(set, f)))
    }

    /// May a part with `info` start at char `start`, after the last part of `s.path`
    /// (`CHECKCOMPOUNDCASE`, `CHECKCOMPOUNDTRIPLE`, `CHECKCOMPOUNDPATTERN`)?
    fn boundary_ok(&self, s: &Search, start: usize, info: &Info) -> bool {
        let Some(&(ps, pe, prev)) = s.path.last() else { return true };
        let c = s.c;
        let (Some(&a), Some(&b)) = (s.chars.get(pe.saturating_sub(1)), s.chars.get(start)) else { return true };
        if c.case && (a.is_uppercase() || b.is_uppercase()) && a != '-' && b != '-' {
            return false;
        }
        if c.triple && start == pe && a == b {
            let before = pe >= 2 && s.chars.get(pe - 2) == Some(&a);
            let after = s.chars.get(start + 1) == Some(&b);
            if before || after {
                return false;
            }
        }
        if c.patterns.is_empty() {
            return true;
        }
        let text = |x: usize, y: usize| s.cuts.get(x).zip(s.cuts.get(y)).and_then(|(&x, &y)| s.w.get(x..y)).unwrap_or_default();
        let (before, after) = (text(ps, pe), text(start, s.chars.len()));
        !c.patterns.iter().any(|p| {
            before.ends_with(&*p.end)
                && after.starts_with(&*p.begin)
                && (!p.unmodified || prev.unmodified)
                && p.end_need.met(prev.marks)
                && p.begin_need.met(info.marks)
        })
    }

    /// Are the parts of `s.path` few enough (`COMPOUNDWORDMAX`, `COMPOUNDROOT`)? `whole`: the
    /// word is complete (the Hungarian syllable rule counts its vowels).
    fn words_ok(&self, s: &Search, whole: bool) -> bool {
        let Some(max) = s.c.word_max else { return true };
        let words: usize = s.path.iter().map(|p| usize::from(p.2.words)).sum();
        if words <= max {
            return true;
        }
        match &s.c.syllables {
            None => false,
            Some(_) if !whole => true,
            Some((most, vowels)) => s.chars.iter().filter(|c| vowels.contains(c)).count() <= *most,
        }
    }

    /// The checks on a complete split: two parts at least, `COMPOUNDWORDMAX`, `CHECKCOMPOUNDDUP`,
    /// `FORCEUCASE`, the three-part rule and `CHECKCOMPOUNDREP`.
    fn whole_ok(&self, s: &mut Search) -> bool {
        let n = s.path.len();
        if n < 2 || !self.words_ok(s, true) {
            return false;
        }
        let (Some(&(_, _, last)), Some(&(_, _, prev))) = (s.path.last(), s.path.get(n - 2)) else { return false };
        if s.c.dup && last.stem == prev.stem {
            return false;
        }
        if last.marks & 1 != 0 && s.c.force_ucase.is_some() && !s.capitalized {
            return false;
        }
        let span = |s: &Search, i: usize, j: usize| -> String {
            let (Some(&(a, ..)), Some(&(_, b, _))) = (s.path.get(i), s.path.get(j)) else { return String::new() };
            s.cuts.get(a).zip(s.cuts.get(b)).and_then(|(&x, &y)| s.w.get(x..y)).unwrap_or_default().to_string()
        };
        if n >= 3 && self.near_word(s) {
            return false;
        }
        if s.c.rep && !self.rep.is_empty() {
            let pairs = (0..n - 1).map(|i| (i, i + 1));
            let tails = (0..n - 1).map(|i| (i, n - 1));
            let spans: Vec<String> = pairs.chain(tails).map(|(i, j)| span(s, i, j)).collect();
            if spans.iter().any(|sp| self.rep_fault(sp, s)) {
                return false;
            }
        }
        true
    }

    /// The check on a compound of three or more parts: is the word one slip away from a word
    /// (not a compound)? Then it is taken for a misspelling of that word. The slips are the
    /// CLI's (black-box, see the tests): a letter too many, a pair typed twice, two letters
    /// swapped or one moved (up to 4 apart), a keyboard neighbour (`KEY`), a `TRY` letter, a
    /// `REP` or `MAP` change, the word capitalized, and (without `NOSPLITSUGS`) two words. It
    /// depends on the word only, so it is answered once per check.
    fn near_word(&self, s: &mut Search) -> bool {
        if let Some(near) = s.near {
            return near;
        }
        let excluded = s.excluded;
        let mut work = s.work;
        let mut is_word = |cand: &[char]| {
            work += 1;
            work > MAX_WORK || self.simple_word(&cand.iter().collect::<String>(), excluded)
        };
        let near = self.near_miss(&s.chars, s.c.two_words, &mut is_word);
        s.work = work;
        s.near = Some(near);
        near
    }

    fn near_miss(&self, w: &[char], two_words: bool, is_word: &mut impl FnMut(&[char]) -> bool) -> bool {
        let n = w.len();
        let mut cand: Vec<char> = Vec::with_capacity(n + 8);
        let mut first = w.iter().take(1).flat_map(|c| c.to_uppercase()).collect::<Vec<char>>();
        first.extend(w.iter().skip(1));
        if is_word(&first) || is_word(&w.iter().flat_map(|c| c.to_uppercase()).collect::<Vec<char>>()) {
            return true;
        }
        for i in 0..n {
            // A letter too many, and a pair typed twice (`abab` for `ab`).
            cand.clear();
            cand.extend(w.iter().take(i).chain(w.iter().skip(i + 1)));
            if is_word(&cand) {
                return true;
            }
            if i + 3 < n && w.get(i) == w.get(i + 2) && w.get(i + 1) == w.get(i + 3) {
                cand.clear();
                cand.extend(w.iter().take(i).chain(w.iter().skip(i + 2)));
                if is_word(&cand) {
                    return true;
                }
            }
            for d in 1..=4 {
                if i + d >= n {
                    break;
                }
                // Swapped with a letter up to 4 further on.
                cand.clear();
                cand.extend_from_slice(w);
                cand.swap(i, i + d);
                if cand != w && is_word(&cand) {
                    return true;
                }
                // Moved 2 to 4 places on or back (one place is a swap).
                if d >= 2 {
                    cand.clear();
                    cand.extend_from_slice(w);
                    for turn in [1, 2] {
                        if let Some(slice) = cand.get_mut(i..=i + d) {
                            // Left by one: `w[i]` moves on; then right by two: `w[i + d]` back.
                            if turn == 1 { slice.rotate_left(1) } else { slice.rotate_right(2) }
                        }
                        if is_word(&cand) {
                            return true;
                        }
                    }
                }
            }
        }
        // One letter for another: a keyboard neighbour, a TRY letter.
        let key = if self.key.is_empty() { DEFAULT_KEY } else { self.key.as_str() };
        let neighbours = |c: char| {
            key.split('|').flat_map(move |row| {
                let row: Vec<char> = row.chars().take(MAX_KEY_ROW).collect();
                let at: Vec<usize> = row.iter().enumerate().filter(|(_, k)| **k == c).map(|(i, _)| i).collect();
                at.into_iter()
                    .flat_map(move |i| [i.checked_sub(1).and_then(|j| row.get(j)).copied(), row.get(i + 1).copied()])
                    .flatten()
                    .collect::<Vec<char>>()
            })
        };
        let tries: Vec<char> = self.try_chars.chars().take(MAX_TRY).collect();
        for i in 0..n {
            let Some(&c) = w.get(i) else { continue };
            for r in neighbours(c).chain(tries.iter().copied()) {
                if r == c {
                    continue;
                }
                cand.clear();
                cand.extend_from_slice(w);
                if let Some(x) = cand.get_mut(i) {
                    *x = r;
                }
                if is_word(&cand) {
                    return true;
                }
            }
        }
        // `REP` (here `^` and `$` tie to the word's start and end) and `MAP` changes.
        let text: String = w.iter().collect();
        for (from, to) in &self.rep {
            let (start, from) = from.strip_prefix('^').map_or((false, from.as_str()), |f| (true, f));
            let (end, from) = from.strip_suffix('$').map_or((false, from), |f| (true, f));
            if from.is_empty() || to.contains(' ') {
                continue;
            }
            for (i, _) in text.match_indices(from) {
                if (start && i != 0) || (end && i + from.len() != text.len()) {
                    continue;
                }
                let changed: Vec<char> = text
                    .get(..i)
                    .unwrap_or_default()
                    .chars()
                    .chain(to.chars())
                    .chain(text.get(i + from.len()..).unwrap_or_default().chars())
                    .collect();
                if is_word(&changed) {
                    return true;
                }
            }
        }
        let mut budget = MAX_MAP_VARIANTS;
        if !self.map.is_empty() && self.map_variant(w, 0, &mut w.to_vec(), &mut budget, is_word) {
            return true;
        }
        two_words
            && (1..n).any(|i| {
                let (head, tail) = w.split_at(i);
                is_word(head) && is_word(tail)
            })
    }

    /// Does changing letters of `cand` from `i` on to others of their `MAP` group make a word?
    fn map_variant(&self, w: &[char], i: usize, cand: &mut Vec<char>, budget: &mut usize, is_word: &mut impl FnMut(&[char]) -> bool) -> bool {
        let Some(&c) = w.get(i) else { return cand.as_slice() != w && is_word(cand) };
        if self.map_variant(w, i + 1, cand, budget, is_word) {
            return true;
        }
        for group in self.map.iter().filter(|g| g.contains(&c)) {
            for &r in group.iter().filter(|&&r| r != c) {
                if *budget == 0 {
                    return false;
                }
                *budget -= 1;
                if let Some(x) = cand.get_mut(i) {
                    *x = r;
                }
                let hit = self.map_variant(w, i + 1, cand, budget, is_word);
                if let Some(x) = cand.get_mut(i) {
                    *x = c;
                }
                if hit {
                    return true;
                }
            }
        }
        false
    }

    /// Does a `REP` replacement turn `text` into a word (not a compound)?
    fn rep_fault(&self, text: &str, s: &mut Search) -> bool {
        let mut cand = String::with_capacity(text.len() + 8);
        for (from, to) in &self.rep {
            // Replacements tied to a word's start or end (`^`, `$`) are for suggestions: the CLI
            // never applies them here.
            if from.is_empty() || from.starts_with('^') || from.ends_with('$') {
                continue;
            }
            for (i, _) in text.match_indices(from.as_str()) {
                s.work += 1;
                if s.work > MAX_WORK {
                    return true;
                }
                cand.clear();
                cand.push_str(text.get(..i).unwrap_or_default());
                cand.push_str(to);
                cand.push_str(text.get(i + from.len()..).unwrap_or_default());
                if self.simple_word(&cand, s.excluded) {
                    return true;
                }
            }
        }
        false
    }
}

/// The compounding directives a compact-format line may hold, read like the `.aff`'s.
pub(super) fn is_directive(dir: &str) -> bool {
    FLAG_DIRECTIVES.contains(&dir)
        || SWITCHES.contains(&dir)
        || matches!(dir, "COMPOUNDWORDMAX" | "COMPOUNDSYLLABLE" | "LANG" | "MAXCPDSUGS" | "NOSPLITSUGS" | "CHECKCOMPOUNDPATTERN")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::affix::AffixDict;

    /// A synthetic dictionary and the `hunspell` 1.7.5 CLI's answers on it (black-box: `hunspell
    /// -a`), for every word of one to three `pieces` and the `extra` words. `listed` are the
    /// right words when `right`, else the wrong ones; every other word is the opposite.
    struct Pinned {
        name: &'static str,
        aff: &'static str,
        dic: &'static str,
        pieces: &'static str,
        extra: &'static str,
        right: bool,
        listed: &'static str,
    }

    fn load(aff: &str, dic: &str) -> AffixDict {
        let entries: Vec<&str> = dic.split_whitespace().collect();
        let dic = format!("{}\n{}\n", entries.len(), entries.join("\n"));
        AffixDict::from_hunspell(&format!("SET UTF-8\n{aff}\n"), &dic, &[]).unwrap()
    }

    fn candidates(p: &Pinned) -> Vec<String> {
        let pieces: Vec<&str> = p.pieces.split_whitespace().collect();
        let mut out: Vec<String> = pieces.iter().map(|a| (*a).to_string()).collect();
        for a in &pieces {
            for b in &pieces {
                out.push(format!("{a}{b}"));
            }
        }
        for a in &pieces {
            for b in &pieces {
                for c in &pieces {
                    out.push(format!("{a}{b}{c}"));
                }
            }
        }
        out.extend(p.extra.split_whitespace().map(str::to_string));
        out
    }

    /// One case per directive (or pair that only means something together), each word's answer
    /// pinned to the CLI's, before and after the compact format.
    #[test]
    fn flag_compounds_answer_as_the_cli_does() {
        let mut failed = Vec::new();
        for p in PINNED {
            let d = load(p.aff, p.dic);
            let back = AffixDict::from_bytes(&d.to_bytes().unwrap()).unwrap();
            let listed: HashSet<&str> = p.listed.split_whitespace().collect();
            for w in candidates(p) {
                let want = listed.contains(w.as_str()) == p.right;
                if d.check(&w) != want || back.check(&w) != want {
                    failed.push(format!("{}: {w} should be {}", p.name, if want { "right" } else { "wrong" }));
                }
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    /// Real dictionaries (the Language packs' `.aff`/`.dic`): words the CLI answers right and
    /// wrong on the same files (black-box; mostly random compounds of the dictionary's forms).
    /// Run when a craft-i18n checkout is in `CRAFT_I18N_DIR` and its harness has fetched the pack
    /// files (`CRAFT_I18N_CACHE`, default `$CRAFT_I18N_DIR/target/language-data-cache`, files named
    /// by their sha256); each language whose files are missing is skipped.
    #[test]
    fn pack_dictionaries_compound_as_the_cli_does() {
        let Some(dir) = std::env::var_os("CRAFT_I18N_DIR").map(std::path::PathBuf::from) else { return };
        let Ok(manifest) = std::fs::read_to_string(dir.join("language-data/manifest.toml")) else { return };
        let cache = std::env::var_os("CRAFT_I18N_CACHE").map_or_else(|| dir.join("target/language-data-cache"), std::path::PathBuf::from);
        let manifest: toml::Table = manifest.parse().unwrap();
        let files = manifest.get("file").and_then(toml::Value::as_array).cloned().unwrap_or_default();
        let read = |locale: &str, kind: &str| {
            let f = files.iter().find(|f| {
                f.get("locale").and_then(toml::Value::as_str) == Some(locale) && f.get("kind").and_then(toml::Value::as_str) == Some(kind)
            })?;
            std::fs::read(cache.join(f.get("sha256")?.as_str()?)).ok()
        };
        for (locale, right, wrong) in REAL {
            let (Some(aff), Some(dic)) = (read(locale, "spelling-aff"), read(locale, "spelling-dic")) else {
                eprintln!("{locale}: pack files not fetched, skipped");
                continue;
            };
            let (aff, dic) = crate::hunspell::decode(&aff, &dic).unwrap();
            let d = AffixDict::from_hunspell(&aff, &dic, &[]).unwrap_or_else(|e| panic!("{locale}: {e}"));
            for w in right.split_whitespace() {
                assert!(d.check(w), "{locale}: {w} is right");
            }
            for w in wrong.split_whitespace() {
                assert!(!d.check(w), "{locale}: {w} is wrong");
            }
        }
    }

    /// (locale, right words, wrong words), the CLI's answers.
    const REAL: &[(&str, &str, &str)] = &[
        (
            "da",
            "sugammadexgårdfacadens kraftudtagssmutterbuttetheden pebermakrellensudvalgsarbejde Helsinkifirbladen",
            "arbejdslagernebegivende visdommensrustet farfarentraveport egyptologiskopladesvoldtoges",
        ),
        (
            "de",
            "Südwestbeifallsturm Machtfeudalideologie Schleifhärtering Gesprächskontur schule",
            "chilisoßeswitttenaus Magmabewegungaminosäurebasis ausgezählterzehntagefieber heerfolgetemperatechnik haus buch",
        ),
        (
            "de-at",
            "Verträglichkeitsnackenleder Sichelmangrovenwäldern Wehrpflichtbeiruter Kühlerblockigelnothilfe",
            "Jungenschulemakromere Dagösnestküken vogtlandbahnelektronenarmem qualitativmedwedew",
        ),
        (
            "de-ch",
            "Königsstahlextensionsfähigkeit Barlachpregelinsel Euroasiendysprosiums Hexendiabasgesteine",
            "eingeglittenswohlbeleibtesten plakatflächewürdigens durchzuwandernliebediener zurückerinnernskolonnadenform",
        ),
        (
            "eo",
            "elipsoidolumŝirmila resonancosonarkivojn bazanumeralosukersciuraj vivspertoellistiĝkapablo",
            "nekapneatsgvidrolosmitozon Alaholaŭpove kapbalanadajsaŭtomatiĝadajn filamentecukrematoriosilviajn",
        ),
        (
            "et",
            "jahvakemgisesski talõššidekäunuvaikski kõhnunuimatessegikräunujaisse nekroosiupsusina",
            "spinningtoismele kollektivismessüngimainagi elektrolüüsispilgutelebki fosforisesverääni",
        ),
        ("fa", "", "بنمایاندمنتراوی زرشکتبهترانملجاجتکارترینید پارامترمبرقصاندیم"),
        ("hr", "truležne", "vaeašteka razuvjeriliuzdanajpsujućemu pilencadiparieovom časomicevoziti"),
        (
            "hu",
            "eurofelsálazásiabbjáéin",
            "síposékéibahetvenegyszerezni BMsorbánocskáérakönigesedéknek antihősekorgalvántelepeitekig Grazcákompendiumnyijaimmal",
        ),
        ("mn", "Канейдианхалифат Балжойрд", "өехийлчхөгсудлагчдархуутайгүй зээнцрүүдийгээбачиг цуглавруудынхаатөвшинт"),
        ("nb", "", "fellesomkostningasweeperrollen kremfløytenalkoholdebatter Elsepålestikka translatøreksamenerkeldgufsene"),
        (
            "nl",
            "pikjesboekbinderspersen samengingengeloofsverdeeldheid sleepliftenerfgoed stortputtenbehadeling",
            "onbeleefdheidhortensiastraat verwindjuilee Lotharengersvolontair gechromeerdsrietpluimen",
        ),
        ("nn", "", "orstuvastavantgardistar påskemåndagarkjempekenguruar piggtrådmusikkarreknemaskinar stimreinselikniårekeiper"),
        (
            "sv",
            "målvärdenservername sunniglädjepirrets kedjelängdendöbattangerna basfärgmetropoliterna",
            "ogenomskådligsbåtsmanstorps tännkarheiskanens stängningstaggensbehängas råseleedemonlägeouppvärmt",
        ),
        (
            "sv-fi",
            "Ausåsciseleringar uppsatsämnesinsatsens pimpelfiskelagstiftningsarbete orderstocksglutenet",
            "smitartäbyeors triialiseringarkronkoloni databasenslågstadiumcardiganen springformsknegarensronneby",
        ),
    ];

    /// `COMPOUNDRULE` as the Swedish (`)k`) and Mongolian (`[a0,a1]`, `%?`, `.`) dictionaries
    /// write it, read as the CLI answers on the same rules.
    #[test]
    fn compound_rules_with_odd_marks_answer_as_the_cli_does() {
        let d = load("COMPOUNDMIN 1\nCOMPOUNDRULE 1\nCOMPOUNDRULE )k", "a/) b/k");
        let back = AffixDict::from_bytes(&d.to_bytes().unwrap()).unwrap();
        for d in [&d, &back] {
            assert!(d.check("ab") && !d.check("abb") && !d.check("aab"));
        }
        let d = load("FLAG long\nCOMPOUNDMIN 1\nCOMPOUNDRULE 1\nCOMPOUNDRULE (aa)*[a0,a1](bb)", "x/aa y/bb");
        assert!(d.check("xy") && d.check("xxy") && !d.check("xyy") && !d.check("yx"));
        for rule in ["(nn)*%?", "(nn)*.(nn)*%?", "(aa)*%?(bb)"] {
            let d = load(&format!("FLAG long\nCOMPOUNDMIN 1\nCOMPOUNDRULE 1\nCOMPOUNDRULE {rule}"), "x/nn y/nn z/aa w/bb");
            let back = AffixDict::from_bytes(&d.to_bytes().unwrap()).unwrap();
            for d in [&d, &back] {
                assert!(!d.check("xy") && !d.check("xxy") && !d.check("zw") && d.check("x"), "{rule}");
            }
        }
    }

    /// The worst dictionary for the search: every letter and pair of letters a part anywhere,
    /// every check on, hundreds of `REP`s, a long `TRY`, `MAP` groups and chained suffixes that
    /// permit compounding. Each word still answers within the work bound.
    #[test]
    fn hostile_compounding_stays_cheap() {
        let letters: Vec<char> = ('a'..='z').collect();
        let mut aff = String::from(
            "COMPOUNDFLAG X\nCOMPOUNDMIN 1\nCOMPOUNDWORDMAX 99\nCOMPOUNDPERMITFLAG P\nONLYINCOMPOUND O\nCHECKCOMPOUNDDUP\nCHECKCOMPOUNDREP\n\
             CHECKCOMPOUNDCASE\nCHECKCOMPOUNDTRIPLE\nSIMPLIFIEDTRIPLE\nTRY abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n\
             MAP 3\nMAP aeiou\nMAP bcdfg\nMAP hjklm\nCHECKCOMPOUNDPATTERN 2\nCHECKCOMPOUNDPATTERN a/X b/X\nCHECKCOMPOUNDPATTERN 0/P c\n",
        );
        aff.push_str(&format!("REP {}\n", letters.len() * letters.len()));
        for a in &letters {
            for b in &letters {
                aff.push_str(&format!("REP {a} {b}{a}\n"));
            }
        }
        for (i, a) in letters.iter().enumerate() {
            aff.push_str(&format!("SFX {a} Y 1\nSFX {a} 0 {a}/XPO{} .\n", letters.get(i + 1).copied().unwrap_or('a')));
        }
        let flags: String = letters.iter().collect();
        let mut dic: Vec<String> = letters.iter().map(|a| format!("{a}/X{flags}")).collect();
        for a in &letters {
            for b in &letters {
                dic.push(format!("{a}{b}/X{flags}"));
            }
        }
        let d = AffixDict::from_hunspell(&format!("SET UTF-8\n{aff}"), &format!("{}\n{}\n", dic.len(), dic.join("\n")), &[]).unwrap();
        let mut rng = 7u64;
        let random: String = (0..120)
            .map(|_| {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                letters.get((rng >> 33) as usize % 26).copied().unwrap_or('a')
            })
            .collect();
        let mut slowest = std::time::Duration::ZERO;
        let mut words: Vec<String> = vec!["a".repeat(120), "ab".repeat(60), random.clone()];
        for n in [8, 16, 24, 32, 40, 48, 64] {
            words.extend([random.chars().take(n).collect(), "a".repeat(n), "ab".repeat(n / 2), "aab".repeat(n / 3)]);
        }
        for w in &words {
            let t = std::time::Instant::now();
            let _ = d.check(w);
            slowest = slowest.max(t.elapsed());
        }
        eprintln!("hostile compounding: slowest word {slowest:?}");
        assert!(slowest.as_secs() < 5, "{slowest:?}");
    }

    const PINNED: &[Pinned] = &[
        Pinned {
            name: "flag",
            aff: "COMPOUNDFLAG X",
            dic: "foo/X bar/X ba/X baz",
            pieces: "foo bar ba baz",
            extra: "",
            right: true,
            listed: "foo bar ba baz foofoo foobar barfoo barbar foofoofoo foofoobar foobarfoo foobarbar barfoofoo barfoobar \
                barbarfoo barbarbar",
        },
        Pinned { name: "min", aff: "COMPOUNDFLAG X\nCOMPOUNDMIN 1", dic: "f/X bar/X", pieces: "f bar", extra: "", right: false, listed: "" },
        Pinned {
            name: "affixes",
            aff: "COMPOUNDFLAG X\nCOMPOUNDPERMITFLAG Z\nSFX S Y 1\nSFX S 0 s .\nPFX P Y 1\nPFX P 0 un .\nSFX T Y 1\nSFX T 0 t/Z .\nPFX Q Y 1\nPFX Q 0 re/Z .",
            dic: "foo/XSPTQ bar/XSPTQ baz/S",
            pieces: "foo foos unfoo foot refoo bar",
            extra: "barunfoo unfoobar barfoot footbar refoobar barrefoo barfoos foosbar baz",
            right: false,
            listed: "foounfoo foosfoo foosfoos foosunfoo foosfoot foosrefoo foosbar unfoounfoo footunfoo refoounfoo barunfoo \
                foofoounfoo foofoosfoo foofoosfoos foofoosunfoo foofoosfoot foofoosrefoo foofoosbar foounfoounfoo foofootunfoo \
                foorefoounfoo foobarunfoo foosfoofoo foosfoofoos foosfoounfoo foosfoofoot foosfoorefoo foosfoobar foosfoosfoo \
                foosfoosfoos foosfoosunfoo foosfoosfoot foosfoosrefoo foosfoosbar foosunfoofoo foosunfoofoos foosunfoounfoo \
                foosunfoofoot foosunfoorefoo foosunfoobar foosfootfoo foosfootfoos foosfootunfoo foosfootfoot foosfootrefoo \
                foosfootbar foosrefoofoo foosrefoofoos foosrefoounfoo foosrefoofoot foosrefoorefoo foosrefoobar foosbarfoo \
                foosbarfoos foosbarunfoo foosbarfoot foosbarrefoo foosbarbar unfoofoounfoo unfoofoosfoo unfoofoosfoos \
                unfoofoosunfoo unfoofoosfoot unfoofoosrefoo unfoofoosbar unfoounfoounfoo unfoofootunfoo unfoorefoounfoo \
                unfoobarunfoo footfoounfoo footfoosfoo footfoosfoos footfoosunfoo footfoosfoot footfoosrefoo footfoosbar \
                footunfoounfoo footfootunfoo footrefoounfoo footbarunfoo refoofoounfoo refoofoosfoo refoofoosfoos \
                refoofoosunfoo refoofoosfoot refoofoosrefoo refoofoosbar refoounfoounfoo refoofootunfoo refoorefoounfoo \
                refoobarunfoo barfoounfoo barfoosfoo barfoosfoos barfoosunfoo barfoosfoot barfoosrefoo barfoosbar \
                barunfoounfoo barfootunfoo barrefoounfoo barbarunfoo",
        },
        Pinned {
            name: "affix_flags",
            aff: "COMPOUNDFLAG X\nSFX A Y 1\nSFX A 0 s/X .\nPFX B Y 1\nPFX B 0 un/X .\nSFX C Y 1\nSFX C 0 en .",
            dic: "arbeit/AC comp/XC haus/B",
            pieces: "arbeit arbeits comp compen haus unhaus",
            extra: "",
            right: true,
            listed: "arbeit arbeits comp compen haus unhaus comparbeits compcomp compcompen unhausarbeits unhauscomp unhauscompen \
                compcomparbeits compcompcomp compcompcompen compunhausarbeits compunhauscomp compunhauscompen \
                unhauscomparbeits unhauscompcomp unhauscompcompen unhausunhausarbeits unhausunhauscomp unhausunhauscompen",
        },
        Pinned {
            name: "positions",
            aff: "COMPOUNDFLAG X\nCOMPOUNDBEGIN B\nCOMPOUNDMIDDLE M\nCOMPOUNDEND E",
            dic: "beg/B mid/M end/E any/X all/BME",
            pieces: "beg mid end any all",
            extra: "",
            right: true,
            listed: "beg mid end any all begend begany begall anyend anyany anyall allend allany allall begmidend begmidany \
                begmidall beganyend beganyany beganyall begallend begallany begallall anymidend anymidany anymidall anyanyend \
                anyanyany anyanyall anyallend anyallany anyallall allmidend allmidany allmidall allanyend allanyany allanyall \
                allallend allallany allallall",
        },
        Pinned {
            name: "first_last",
            aff: "COMPOUNDFIRST B\nCOMPOUNDLAST E",
            dic: "beg/B end/E",
            pieces: "beg end",
            extra: "",
            right: true,
            listed: "beg end",
        },
        Pinned {
            name: "onlyincompound",
            aff: "COMPOUNDFLAG X\nONLYINCOMPOUND O\nSFX A Y 1\nSFX A 0 er/B .\nSFX B Y 1\nSFX B 0 s .\nSFX F Y 1\nSFX F 0 s/O .",
            dic: "foo/XAF bar/XAF zap/XO qux/A",
            pieces: "foo foos fooers bar zap",
            extra: "zap foos fooer fooers quxer quxers",
            right: true,
            listed: "foo fooers bar foofoo foofooers foobar foozap barfoo barfooers barbar barzap zapfoo zapfooers zapbar zapzap \
                foofoofoo foofoofooers foofoobar foofoozap foobarfoo foobarfooers foobarbar foobarzap foozapfoo foozapfooers \
                foozapbar foozapzap barfoofoo barfoofooers barfoobar barfoozap barbarfoo barbarfooers barbarbar barbarzap \
                barzapfoo barzapfooers barzapbar barzapzap zapfoofoo zapfoofooers zapfoobar zapfoozap zapbarfoo zapbarfooers \
                zapbarbar zapbarzap zapzapfoo zapzapfooers zapzapbar zapzapzap fooer quxer quxers",
        },
        Pinned {
            name: "onlyincompound_permit",
            aff: "COMPOUNDFLAG X\nONLYINCOMPOUND O\nCOMPOUNDPERMITFLAG P\nSFX F Y 1\nSFX F 0 s/OP .\nSFX G Y 1\nSFX G 0 s/O .\nPFX H Y 1\nPFX H 0 un/O .\nPFX I Y 1\nPFX I 0 re/OP .",
            dic: "foo/XFHI bar/XFHI baz/XG",
            pieces: "foo foos unfoo refoo bazs",
            extra: "foos unfoo refoo bazs baz",
            right: true,
            listed: "foo foofoo foorefoo foosfoo foosrefoo unfoofoo unfoorefoo refoofoo refoorefoo foofoofoo foofoorefoo foofoosfoo \
                foofoosrefoo foounfoofoo foounfoorefoo foorefoofoo foorefoorefoo foosfoofoo foosfoorefoo foosfoosfoo \
                foosfoosrefoo foosunfoofoo foosunfoorefoo foosrefoofoo foosrefoorefoo unfoofoofoo unfoofoorefoo unfoofoosfoo \
                unfoofoosrefoo unfoounfoofoo unfoounfoorefoo unfoorefoofoo unfoorefoorefoo refoofoofoo refoofoorefoo \
                refoofoosfoo refoofoosrefoo refoounfoofoo refoounfoorefoo refoorefoofoo refoorefoorefoo baz",
        },
        Pinned {
            name: "forbid_needaffix",
            aff: "COMPOUNDFLAG X\nCOMPOUNDFORBIDFLAG Q\nCOMPOUNDPERMITFLAG P\nNEEDAFFIX N\nSFX S Y 1\nSFX S 0 s .\nSFX T Y 1\nSFX T 0 t/Q .\nSFX U Y 1\nSFX U 0 u/QP .\nSFX V Y 1\nSFX V 0 v/P .",
            dic: "foo/XSTUV bar/XSTUV qoo/XQS nee/XNSV noo/NSV",
            pieces: "foo foot foou qoo nee nees neev",
            extra: "nee noo noos",
            right: true,
            listed: "foo foot foou qoo nees neev foofoo foofoot foofoou fooqoo foonees fooneev neevfoo neevfoot neevfoou neevqoo \
                neevnees neevneev foofoofoo foofoofoot foofoofoou foofooqoo foofoonees foofooneev fooneevfoo fooneevfoot \
                fooneevfoou fooneevqoo fooneevnees fooneevneev neevfoofoo neevfoofoot neevfoofoou neevfooqoo neevfoonees \
                neevfooneev neevneevfoo neevneevfoot neevneevfoou neevneevqoo neevneevnees neevneevneev noos",
        },
        Pinned {
            name: "dup",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDDUP\nSFX S Y 1\nSFX S 0 s .",
            dic: "foo/XS bar/XS foos/X",
            pieces: "foo foos bar",
            extra: "",
            right: false,
            listed: "foofoo barbar foofoofoo foobarbar foosfoofoo foosbarbar barfoofoo barbarbar",
        },
        Pinned {
            name: "case",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDCASE",
            dic: "foo/X Bar/X baz/X QUX/X",
            pieces: "foo Bar baz QUX",
            extra: "",
            right: true,
            listed: "foo Bar baz QUX foofoo foobaz Barfoo Barbaz bazfoo bazbaz foofoofoo foofoobaz foobazfoo foobazbaz Barfoofoo \
                Barfoobaz Barbazfoo Barbazbaz bazfoofoo bazfoobaz bazbazfoo bazbazbaz",
        },
        Pinned {
            name: "triple",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDTRIPLE",
            dic: "foo/X bar/X schiff/X fahrt/X ooze/X baa/X",
            pieces: "foo schiff fahrt ooze baa",
            extra: "",
            right: false,
            listed: "fooooze schifffoo schifffahrt foofooooze fooschifffoo fooschifffahrt foooozefoo foooozeschiff foooozefahrt \
                foooozeooze foooozebaa schifffoofoo schifffooschiff schifffoofahrt schifffooooze schifffoobaa schiffschifffoo \
                schiffschifffahrt schifffahrtfoo schifffahrtschiff schifffahrtfahrt schifffahrtooze schifffahrtbaa \
                fahrtfooooze fahrtschifffoo fahrtschifffahrt oozefooooze oozeschifffoo oozeschifffahrt baafooooze baaschifffoo \
                baaschifffahrt",
        },
        Pinned {
            name: "simplified_triple",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDTRIPLE\nSIMPLIFIEDTRIPLE",
            dic: "foo/X bar/X schiff/X fahrt/X ooze/X baa/X",
            pieces: "foo schiff fahrt ooze baa",
            extra: "schiffahrt",
            right: false,
            listed: "fooooze schifffoo schifffahrt foofooooze fooschifffoo fooschifffahrt foooozefoo foooozeschiff foooozefahrt \
                foooozeooze foooozebaa schifffoofoo schifffooschiff schifffoofahrt schifffooooze schifffoobaa schiffschifffoo \
                schiffschifffahrt schifffahrtfoo schifffahrtschiff schifffahrtfahrt schifffahrtooze schifffahrtbaa \
                fahrtfooooze fahrtschifffoo fahrtschifffahrt oozefooooze oozeschifffoo oozeschifffahrt baafooooze baaschifffoo \
                baaschifffahrt",
        },
        Pinned {
            name: "simplified_only",
            aff: "COMPOUNDFLAG X\nSIMPLIFIEDTRIPLE",
            dic: "foo/X schiff/X fahrt/X ooze/X",
            pieces: "foo schiff fahrt ooze",
            extra: "schiffahrt fooze",
            right: false,
            listed: "fooze",
        },
        Pinned {
            name: "rep",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDREP\nREP 4\nREP oo o\nREP rb r\nREP ^ba x\nREP az$ x\nSFX S Y 1\nSFX S 0 s .",
            dic: "foo/XS bar/XS baz/X fobar/S barb/X baraz/S fobaz bx",
            pieces: "foo foos bar bars baz barb",
            extra: "",
            right: true,
            listed: "foo foos bar bars baz barb foofoo foofoos foobarb barfoo barfoos barbar barbars barbarb bazfoo bazfoos bazbar \
                bazbars bazbaz bazbarb barbfoo barbfoos barbbar barbbars barbbaz barbbarb foofoofoo foofoofoos foofoobarb \
                foobarbfoo foobarbfoos foobarbbar foobarbbars foobarbbaz foobarbbarb barfoofoo barfoofoos barfoobarb barbarfoo \
                barbarfoos barbarbar barbarbars barbarbarb barbarbfoo barbarbfoos barbarbbar barbarbbars barbarbbaz \
                barbarbbarb bazfoofoo bazfoofoos bazfoobarb bazbarfoo bazbarfoos bazbarbar bazbarbars bazbarbarb bazbazfoo \
                bazbazfoos bazbazbar bazbazbars bazbazbaz bazbazbarb bazbarbfoo bazbarbfoos bazbarbbar bazbarbbars bazbarbbaz \
                bazbarbbarb barbfoofoo barbfoofoos barbfoobarb barbbarfoo barbbarfoos barbbarbar barbbarbars barbbarbarb \
                barbbazfoo barbbazfoos barbbazbar barbbazbars barbbazbaz barbbazbarb barbbarbfoo barbbarbfoos barbbarbbar \
                barbbarbbars barbbarbbaz barbbarbbarb",
        },
        Pinned {
            name: "rep_spans",
            aff: "COMPOUNDFLAG X\nCHECKCOMPOUNDREP\nREP 1\nREP oo o",
            dic: "foo/X bar/X baz/X qux/X fobarbaz quxfobar",
            pieces: "foo bar baz qux",
            extra: "",
            right: false,
            listed: "foobarbaz quxfoobar",
        },
        Pinned {
            name: "three_parts",
            aff: "COMPOUNDFLAG X",
            dic: "foo/X bar/X baz/X qux/X foobar fooba barbazqux",
            pieces: "foo bar baz qux",
            extra: "fooxbarbaz",
            right: false,
            listed: "foofoobar foobarfoo foobarbar foobarbaz foobarqux barfoobar bazfoobar bazbarqux quxfoobar fooxbarbaz",
        },
        Pinned {
            name: "three_parts_nosplitsugs",
            aff: "COMPOUNDFLAG X\nNOSPLITSUGS",
            dic: "foo/X bar/X baz/X qux/X foobar fooba",
            pieces: "foo bar baz qux",
            extra: "",
            right: false,
            listed: "",
        },
        Pinned {
            name: "pattern",
            aff: "COMPOUNDFLAG X\nCOMPOUNDPERMITFLAG P\nCHECKCOMPOUNDPATTERN 3\nCHECKCOMPOUNDPATTERN oo ba # comment\nCHECKCOMPOUNDPATTERN /F /G\nCHECKCOMPOUNDPATTERN er /H\nSFX S Y 1\nSFX S 0 s/P .\nSFX T Y 1\nSFX T 0 t/PF .\nSFX E Y 1\nSFX E 0 er/P .",
            dic: "foo/XST bar/XSG baz/XS qux/XF zed/XH hey/XSE heyer/XS",
            pieces: "foo foot bar baz qux zed heyer hey",
            extra: "",
            right: false,
            listed: "foobar foobaz footbar quxbar heyerzed foofoobar foofoobaz foofootbar foobarfoo foobarfoot foobarbar foobarbaz \
                foobarqux foobarzed foobarheyer foobarhey foobazfoo foobazfoot foobazbar foobazbaz foobazqux foobazzed \
                foobazheyer foobazhey fooquxbar fooheyerzed footfoobar footfoobaz footfootbar footbarfoo footbarfoot \
                footbarbar footbarbaz footbarqux footbarzed footbarheyer footbarhey footquxbar footheyerzed barfoobar \
                barfoobaz barfootbar barquxbar barheyerzed bazfoobar bazfoobaz bazfootbar bazquxbar bazheyerzed quxfoobar \
                quxfoobaz quxfootbar quxbarfoo quxbarfoot quxbarbar quxbarbaz quxbarqux quxbarzed quxbarheyer quxbarhey \
                quxquxbar quxheyerzed zedfoobar zedfoobaz zedfootbar zedquxbar zedheyerzed heyerfoobar heyerfoobaz \
                heyerfootbar heyerquxbar heyerzedfoo heyerzedfoot heyerzedbar heyerzedbaz heyerzedqux heyerzedzed \
                heyerzedheyer heyerzedhey heyerheyerzed heyfoobar heyfoobaz heyfootbar heyquxbar heyheyerzed",
        },
        Pinned {
            name: "pattern_unmodified_forceucase",
            aff: "COMPOUNDFLAG X\nFORCEUCASE U\nCHECKCOMPOUNDPATTERN 1\nCHECKCOMPOUNDPATTERN 0/A b\nCOMPOUNDPERMITFLAG P\nSFX S Y 1\nSFX S 0 s .\nSFX R Y 1\nSFX R 0 r/P .",
            dic: "foo/XSR straat/XUS bar/X zoo/XAR big/X",
            pieces: "foo straat bar zoo zoor big",
            extra: "Foostraat FOOSTRAAT Foostraats",
            right: false,
            listed: "foostraat straatstraat barstraat zoostraat zoobar zoobig zoorstraat bigstraat foofoostraat foostraatstraat \
                foobarstraat foozoostraat foozoobar foozoobig foozoorstraat foobigstraat straatfoostraat straatstraatstraat \
                straatbarstraat straatzoostraat straatzoobar straatzoobig straatzoorstraat straatbigstraat barfoostraat \
                barstraatstraat barbarstraat barzoostraat barzoobar barzoobig barzoorstraat barbigstraat zoofoostraat \
                zoostraatstraat zoobarfoo zoobarstraat zoobarbar zoobarzoo zoobarzoor zoobarbig zoozoostraat zoozoobar \
                zoozoobig zoozoorstraat zoobigfoo zoobigstraat zoobigbar zoobigzoo zoobigzoor zoobigbig zoorfoostraat \
                zoorstraatstraat zoorbarstraat zoorzoostraat zoorzoobar zoorzoobig zoorzoorstraat zoorbigstraat bigfoostraat \
                bigstraatstraat bigbarstraat bigzoostraat bigzoobar bigzoobig bigzoorstraat bigbigstraat",
        },
        Pinned {
            name: "wordmax_root",
            aff: "COMPOUNDFLAG X\nCOMPOUNDWORDMAX 2\nCOMPOUNDROOT R",
            dic: "foo/X bar/X foobaz/XR",
            pieces: "foo bar foobaz",
            extra: "",
            right: true,
            listed: "foo bar foobaz foofoo foobar barfoo barbar",
        },
        Pinned {
            name: "wordmax",
            aff: "COMPOUNDFLAG X\nCOMPOUNDWORDMAX 3\nCOMPOUNDMIN 1",
            dic: "fo/X ba/X",
            pieces: "fo ba",
            extra: "fobafoba fobafobafo",
            right: false,
            listed: "fobafoba fobafobafo",
        },
        Pinned {
            name: "syllable",
            aff: "COMPOUNDFLAG X\nCOMPOUNDWORDMAX 2\nCOMPOUNDSYLLABLE 6 aeiou\nCOMPOUNDMIN 1",
            dic: "ba/X be/X baaa/X",
            pieces: "ba be baaa",
            extra: "babababa",
            right: false,
            listed: "",
        },
        Pinned {
            name: "syllable_hu",
            aff: "LANG hu_HU\nCOMPOUNDFLAG X\nCOMPOUNDWORDMAX 2 y\nCOMPOUNDSYLLABLE 6 aeiou\nCOMPOUNDMIN 1",
            dic: "ba/X be/X baaa/X",
            pieces: "ba be baaa",
            extra: "babababa",
            right: false,
            listed: "babaaabaaa bebaaabaaa baaababaaa baaabebaaa baaabaaaba baaabaaabe baaabaaabaaa",
        },
        Pinned {
            name: "near_slips",
            aff: "COMPOUNDFLAG X\nCOMPOUNDMIN 4\nTRY x\nREP 1\nREP ^ab yz\nMAP 1\nMAP eë",
            dic: "abcd/X efgh/X ijkl/X mnop/X abcdefghikjl efghabcdjkli ijklabcdefgg xnopabcdefgh yzcdmnopijkl ëfghmnopabcd Ijklmnopabcd",
            pieces: "abcd efgh ijkl mnop",
            extra: "",
            right: false,
            listed: "abcdefghijkl abcdmnopijkl efghabcdijkl efghmnopabcd ijklabcdefgh ijklmnopabcd mnopabcdefgh",
        },
        Pinned {
            name: "forbidden",
            aff: "COMPOUNDFLAG X\nFORBIDDENWORD F\nSFX S Y 1\nSFX S 0 en .",
            dic: "foo/X brok/XS broken/F bar/XF foobar/F",
            pieces: "foo brok broken bar",
            extra: "foobar broken",
            right: true,
            listed: "foo brok foofoo foobrok foobroken brokfoo brokbrok brokbroken foofoofoo foofoobrok foofoobroken foobrokfoo \
                foobrokbrok foobrokbroken brokfoofoo brokfoobrok brokfoobroken brokbrokfoo brokbrokbrok brokbrokbroken",
        },
    ];
}
