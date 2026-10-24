//! Patterns and inputs meant to hang or crash a backtracker. Each must end in `Ok` or `Err`,
//! never a panic, within 100 ms (with WordCraft's 10M-step budget). The workspace builds this
//! crate optimized even in the dev profile so these timings hold under `cargo test`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use craft_regex::{Budget, Error, Options, Regex};
use proptest::prelude::*;

const BUDGET: u64 = 10_000_000;

/// Timed sections take turns, so parallel test threads don't slow each other down.
static TURN: Mutex<()> = Mutex::new(());

/// Compiles and searches, returning the outcome and asserting it took under 100 ms.
fn run(p: &str, o: &Options, hay: &str) -> Result<bool, Error> {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let t = Instant::now();
    let out = Regex::new(p, o).and_then(|r| r.captures_at(hay, 0..hay.len(), &mut Budget::new(BUDGET)).map(|c| c.is_some()));
    let took = t.elapsed();
    assert!(took < Duration::from_millis(100), "{p:.40}… took {took:?}");
    out
}

fn default(p: &str, hay: &str) -> Result<bool, Error> {
    run(p, &Options::default(), hay)
}

fn big() -> Options<'static> {
    Options { max_pattern: 1_000_000, max_insts: 10_000_000, max_depth: 1_000_000, ..Options::default() }
}

#[test]
fn catastrophic_backtracking() {
    let hay = format!("{}b", "a".repeat(40));
    assert_eq!(default("(a+)+$", &hay), Err(Error::TooComplex));
    assert_eq!(default("(a|a)*c", &hay), Err(Error::TooComplex));
    assert_eq!(default("(a|aa)*c", &hay), Err(Error::TooComplex));
    assert_eq!(default("(?:a*)*b", &hay), Ok(true));
    assert_eq!(default("(a++)+$", &hay), Ok(false), "possessive stops the blowup");
}

#[test]
fn recursion_without_base_case() {
    assert_eq!(default("(?R)", "abc"), Err(Error::TooComplex));
    assert_eq!(default("a(?R)", &"a".repeat(5_000)), Err(Error::TooComplex));
    assert_eq!(default("(a(?1))", "aaaa"), Ok(false), "runs out of input first");
    assert_eq!(run("(?R)", &big(), "abc"), Err(Error::TooComplex), "the stack cap still holds");
}

#[test]
fn deep_nesting() {
    let deep = |n: usize| format!("{}a{}", "(".repeat(n), ")".repeat(n));
    assert_eq!(default(&deep(1_000), "a"), Err(Error::TooComplex), "pattern too long");
    assert_eq!(run(&deep(1_000), &Options { max_pattern: 10_000, ..Options::default() }, "a"), Err(Error::TooComplex));
    assert_eq!(run(&deep(999), &Options { max_pattern: 10_000, ..Options::default() }, "a"), Ok(true));
    assert_eq!(run(&deep(50_000), &big(), "a"), Ok(true), "no Rust recursion anywhere");
    let deep_nc = format!("{}a{}", "(?:".repeat(50_000), ")*".repeat(50_000));
    let _ = run(&deep_nc, &big(), "aaa");
    let deep_class = format!("{}[a]{}", "(?=".repeat(20_000), ")".repeat(20_000));
    assert_eq!(run(&deep_class, &big(), "a"), Ok(true));
}

#[test]
fn many_alternatives() {
    let alts = vec!["a"; 10_000].join("|");
    assert_eq!(default(&alts, "a"), Err(Error::TooComplex), "pattern too long");
    let o = Options { max_pattern: 100_000, ..Options::default() };
    assert_eq!(run(&alts, &o, "a"), Err(Error::TooComplex), "too many instructions");
    let o = Options { max_pattern: 100_000, max_insts: 100_000, ..Options::default() };
    assert_eq!(run(&format!("(?:{alts})c"), &o, "aab"), Ok(false));
}

#[test]
fn huge_counted_repeats() {
    assert_eq!(default("(?:a{1000}){1000}", "a"), Err(Error::TooComplex));
    assert_eq!(default("a{65535}", "a"), Err(Error::TooComplex));
    assert!(matches!(default("a{65536}", "a"), Err(Error::Syntax { .. })));
    assert!(matches!(default("a{99999999999}", "a"), Err(Error::Syntax { .. })));
}

#[test]
fn stack_cap() {
    let o = Options { max_stack: 1_000, ..Options::default() };
    assert_eq!(run("a*b", &o, &"a".repeat(5_000)), Err(Error::TooComplex));
    assert_eq!(run("(?:a|b)*c", &o, &"ab".repeat(5_000)), Err(Error::TooComplex));
}

#[test]
fn bad_ranges() {
    let r = Regex::new("x*", &Options::default()).unwrap();
    let mut b = Budget::new(1_000);
    #[allow(clippy::reversed_empty_ranges)]
    for range in [5..2, 0..99, 1..2, 2..3, 99..100, usize::MAX..usize::MAX] {
        assert_eq!(r.captures_at("aé", range.clone(), &mut b), Ok(None), "{range:?}");
    }
    assert!(r.captures_at("", 0..0, &mut b).unwrap().is_some());
}

#[test]
fn zero_budget() {
    let r = Regex::new("a", &Options::default()).unwrap();
    assert_eq!(r.captures_at("a", 0..1, &mut Budget::new(0)), Err(Error::TooComplex));
}

fn junk() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("("),
        Just(")"),
        Just("(?"),
        Just("(?<"),
        Just("(?("),
        Just("(?R)"),
        Just("(?1)"),
        Just("(?-1)"),
        Just("(?="),
        Just("(?<="),
        Just("(?!"),
        Just("(?>"),
        Just("(?|"),
        Just("(?i)"),
        Just("(?x)"),
        Just("["),
        Just("]"),
        Just("[^"),
        Just("[:alpha:]"),
        Just("-"),
        Just("|"),
        Just("*"),
        Just("+"),
        Just("?"),
        Just("{2}"),
        Just("{1,"),
        Just("}"),
        Just("\\"),
        Just("\\1"),
        Just("\\K"),
        Just("\\Q"),
        Just("\\E"),
        Just("\\b"),
        Just("\\p{"),
        Just("\\x{"),
        Just("^"),
        Just("$"),
        Just("."),
        Just("a"),
        Just("é"),
        Just("<n>"),
        Just("&n)"),
        Just("'"),
        Just("#"),
        Just(" "),
        Just("\n"),
        Just("DEFINE)"),
    ];
    proptest::collection::vec(piece, 0..24).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5_000))]

    #[test]
    fn junk_never_panics(p in junk(), hay in "[aé()\\n ]{0,20}", start in 0usize..25, end in 0usize..25) {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let t = Instant::now();
        if let Ok(r) = Regex::new(&p, &Options::default()) {
            let _ = r.captures_at(&hay, start..end, &mut Budget::new(100_000));
            let _ = r.captures_at(&hay, 0..hay.len(), &mut Budget::new(100_000));
        }
        prop_assert!(t.elapsed() < Duration::from_millis(100), "{:?}", p);
    }
}
