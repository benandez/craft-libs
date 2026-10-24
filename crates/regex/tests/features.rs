//! Hand-written cases for each feature. Written from the PCRE2 documentation, not its tests.

use craft_regex::{Budget, Error, Options, Regex};

fn re(p: &str) -> Regex {
    Regex::new(p, &Options::default()).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Every group of the first match, as text.
fn caps<'h>(p: &str, hay: &'h str) -> Option<Vec<Option<&'h str>>> {
    let r = re(p);
    let c = r.captures_at(hay, 0..hay.len(), &mut Budget::new(1_000_000)).unwrap()?;
    Some((0..c.len()).map(|i| c.get(i).map(|g| &hay[g])).collect())
}

/// The first match, as text.
fn m<'h>(p: &str, hay: &'h str) -> Option<&'h str> {
    caps(p, hay).and_then(|c| c[0])
}

fn err(p: &str) -> Error {
    Regex::new(p, &Options::default()).unwrap_err()
}

#[test]
fn literals_and_alternation() {
    assert_eq!(m("abc", "xxabcxx"), Some("abc"));
    assert_eq!(m("a|ab", "ab"), Some("a"), "leftmost-first, not longest");
    assert_eq!(m("", "abc"), Some(""));
    assert_eq!(m("x", "abc"), None);
    assert_eq!(m("é+", "caféé!"), Some("éé"));
}

#[test]
fn quantifiers() {
    assert_eq!(m("a*", "aaa"), Some("aaa"));
    assert_eq!(m("a*?", "aaa"), Some(""));
    assert_eq!(m("a+?", "aaa"), Some("a"));
    assert_eq!(m("a{2}", "aaa"), Some("aa"));
    assert_eq!(m("a{2,}", "aaaa"), Some("aaaa"));
    assert_eq!(m("a{1,3}", "aaaa"), Some("aaa"));
    assert_eq!(m("a{1,3}?", "aaaa"), Some("a"));
    assert_eq!(m("a{,3}", "a{,3}"), Some("a{,3}"), "not a quantifier: literal");
    assert_eq!(m("x{", "x{"), Some("x{"));
    assert_eq!(m("(a?)*b", "aab"), Some("aab"));
    assert_eq!(m("(?:a*)*b", "aab"), Some("aab"), "empty iterations end the loop");
    assert_eq!(m("(?:)*x", "x"), Some("x"));
}

#[test]
fn possessive_and_atomic() {
    assert_eq!(m("a*+a", "aaa"), None);
    assert_eq!(m("a++b", "aaab"), Some("aaab"));
    assert_eq!(m("(?>a+)a", "aaa"), None);
    assert_eq!(m("(?>a|ab)c", "abc"), None, "atomic keeps its first choice");
    assert_eq!(m("(?:a|ab)c", "abc"), Some("abc"));
    assert_eq!(caps("(?>(a+))b", "aab"), Some(vec![Some("aab"), Some("aa")]), "captures survive the cut");
    assert_eq!(m("\"[^\"]*+\"", "say \"hi\" ok"), Some("\"hi\""));
}

#[test]
fn groups_and_names() {
    assert_eq!(caps("(a)(b)?(c)", "ac"), Some(vec![Some("ac"), Some("a"), None, Some("c")]));
    assert_eq!(caps("(?:a)(b)", "ab"), Some(vec![Some("ab"), Some("b")]));
    assert_eq!(caps("(a|b)+", "ab"), Some(vec![Some("ab"), Some("b")]), "last iteration");
    let r = re("(?<year>\\d{4})-(?'month'\\d\\d)-(?P<day>\\d\\d)");
    let hay = "on 2026-10-24.";
    let c = r.captures_at(hay, 0..hay.len(), &mut Budget::new(10_000)).unwrap().unwrap();
    assert_eq!(&hay[c.name("year").unwrap()], "2026");
    assert_eq!(&hay[c.name("month").unwrap()], "10");
    assert_eq!(&hay[c.name("day").unwrap()], "24");
    assert_eq!(c.name("nope"), None);
    let names: Vec<_> = r.group_names().collect();
    assert_eq!(names, vec![(3, "day"), (2, "month"), (1, "year")]);
    assert_eq!(r.captures_len(), 4);
    assert_eq!(caps("(?|(a)|(b))(c)", "bc"), Some(vec![Some("bc"), Some("b"), Some("c")]), "branch reset");
    assert_eq!(caps("(?n)(a)(?<x>b)", "ab"), Some(vec![Some("ab"), Some("b")]));
}

#[test]
fn backreferences() {
    assert_eq!(m(r"\b(\w+) \1\b", "this is is a test"), Some("is is"), "doubled word");
    assert_eq!(m(r"\b(\w+) \1\b", "this is island"), None);
    assert_eq!(caps(r#"(["']).*?\1"#, r#"he said "it's fine" ok"#), Some(vec![Some(r#""it's fine""#), Some("\"")]));
    assert_eq!(m(r"(a)\g1\g{1}\g{-1}", "aaaa"), Some("aaaa"));
    assert_eq!(m(r"(?<q>x)\k<q>\k'q'\k{q}(?P=q)", "xxxxx"), Some("xxxxx"));
    assert_eq!(m(r"(?i)(ab)\1", "abAB"), Some("abAB"));
    assert_eq!(m(r"(ab)\1", "abAB"), None);
    assert_eq!(m(r"(a)|\1", "b"), None, "unset group never matches");
    assert_eq!(m(r"(a|b\1)+", "aba"), Some("aba"), "refers to the previous iteration");
    assert_eq!(m(r"(\w)\1", "Straße mit Kaffee"), Some("ff"));
}

#[test]
fn lookaround() {
    assert_eq!(m(r"(?<=Doe, )J\w+", "Doe, John and Roe, Jane"), Some("John"));
    assert_eq!(m(r"\d+(?= pt)", "12 px 14 pt"), Some("14"));
    assert_eq!(m(r"(?<!\.)\.\.(?!\.)", "a... b.. c"), Some(".."));
    assert_eq!(caps(r"(?<!\.)\.\.(?!\.)", "a... b.. c").map(|_| ()), Some(()));
    assert_eq!(m(r"(?<=\d{1,3})x", "12x"), Some("x"), "bounded variable lookbehind");
    assert_eq!(m(r"(?<=ab|c)d", "abd"), Some("d"));
    assert_eq!(m(r"(?<=ab|c)d", "bd"), None);
    assert_eq!(m(r"(?<!^)x", "xx"), Some("x"));
    assert_eq!(caps(r"(?=(\w+))\w", "ab"), Some(vec![Some("a"), Some("ab")]), "positive lookahead keeps captures");
    assert_eq!(caps(r"(?!(a)b)\w", "ac"), Some(vec![Some("a"), None]), "negative lookahead drops captures");
    assert_eq!(m(r"(?<=é)x", "éx"), Some("x"));
    assert_eq!(m(r"(?<!a|bc)c", "bc"), Some("c"), "a body that ends elsewhere doesn't count");
    assert_eq!(m(r"(?<!a|bc)d", "bcd"), None);
    assert_eq!(m(r"^.(?(?<!a|bc)c|x)", "bc"), Some("bc"));
    assert!(matches!(err(r"(?<=a+)b"), Error::Syntax { .. }));
    assert!(matches!(err(r"(?<=a{256})b"), Error::Syntax { .. }));
    assert!(Regex::new(r"(?<=a{255})b", &Options::default()).is_ok());
}

#[test]
fn recursion() {
    let brackets = r"\((?:[^()]++|(?R))*\)";
    assert_eq!(m(brackets, "x (a(b)c(d(e))) y"), Some("(a(b)c(d(e)))"));
    assert_eq!(m(brackets, "((("), None);
    let quotes = r"«(?:[^«»]++|(?R))*»";
    assert_eq!(m(quotes, "he said «she said «yes» then» ok"), Some("«she said «yes» then»"));
    assert_eq!(m(r"^(a|b(?1)c)$", "bbacc"), Some("bbacc"));
    assert_eq!(m(r"(?<p>\((?:[^()]|(?&p))*\))", "((a))"), Some("((a))"));
    assert_eq!(m(r"(?P<p>x(?P>p)?y)", "xxyy"), Some("xxyy"));
    assert_eq!(m(r"(a)(?-1)(?+1)(b)", "aabb"), Some("aabb"));
    assert_eq!(m(r"(?1)(a|b)", "ba"), Some("ba"), "forward subroutine call");
    // PCRE2: groups set inside a recursion revert when it returns.
    assert_eq!(caps(r"(a)(?1)", "ab"), None);
    assert_eq!(caps(r"(.)(?1)", "ab"), Some(vec![Some("ab"), Some("a")]));
    assert_eq!(caps(r"(?:(a)|b)(?R)?", "ba"), Some(vec![Some("ba"), None]));
    // The group around a palindrome keeps the outer value.
    assert_eq!(caps(r"^((.)(?:(?1)|.?)\2)$", "abcba"), Some(vec![Some("abcba"), Some("abcba"), Some("a")]));
}

#[test]
fn conditionals() {
    let p = r"^(\()?\d+(?(1)\))$";
    assert_eq!(m(p, "(12)"), Some("(12)"));
    assert_eq!(m(p, "12"), Some("12"));
    assert_eq!(m(p, "(12"), None);
    assert_eq!(m(r"^(?<o><)?a(?(<o>)>|!)$", "<a>"), Some("<a>"));
    assert_eq!(m(r"^(?<o><)?a(?('o')>|!)$", "a!"), Some("a!"));
    assert_eq!(m(r"^(?(?=\d)\d+|[a-z]+)$", "123"), Some("123"));
    assert_eq!(m(r"^(?(?=\d)\d+|[a-z]+)$", "abc"), Some("abc"));
    assert_eq!(m(r"^(?(?!\d)[a-z]+|\d+)$", "abc"), Some("abc"));
    assert_eq!(m(r"^(?(?!\d)[a-z]+|\d+)$", "42"), Some("42"));
    assert_eq!(m(r"^(?(?<=x)a|b)", "b"), Some("b"));
    assert_eq!(m(r"(?(?=a)ab|ac)", "ac"), None, "only the chosen branch runs");
    assert_eq!(m(r"(?(?=a)ab|ac)", "ab"), Some("ab"));
    assert_eq!(m(r"(\((?:[^()]|(?1))*\))(?(R)x|y)", "(a)y"), Some("(a)y"));
    assert_eq!(m(r"(?(DEFINE)(?<byte>25[0-5]|2[0-4]\d|1?\d?\d))^(?&byte)(?:\.(?&byte)){3}$", "192.168.0.255"), Some("192.168.0.255"));
    assert_eq!(m(r"(?(DEFINE)(?<byte>25[0-5]|2[0-4]\d|1?\d?\d))^(?&byte)(?:\.(?&byte)){3}$", "192.168.0.256"), None);
    assert!(matches!(err(r"(?(1)a|b|c)"), Error::Syntax { .. }));
}

#[test]
fn keep() {
    assert_eq!(m(r"foo\Kbar", "foobar"), Some("bar"));
    assert_eq!(caps(r"(foo)\Kbar", "foobar"), Some(vec![Some("bar"), Some("foo")]));
    assert!(matches!(err(r"(?=a\K)"), Error::Syntax { .. }));
}

#[test]
fn flags() {
    assert_eq!(m("(?i)straße", "STRAßE"), Some("STRAßE"));
    assert_eq!(m("(?i)strasse", "straße"), None, "simple folding: ß is not ss");
    assert_eq!(m("(?i)k", "\u{212A}"), Some("\u{212A}"), "Kelvin sign folds to k");
    assert_eq!(m("a(?i)b|c", "C"), Some("C"), "flag carries into later branches");
    assert_eq!(m("(?i:a)b", "Ab"), Some("Ab"));
    assert_eq!(m("(?i:a)b", "AB"), None);
    assert_eq!(m("(?i)a(?-i)b", "Ab"), Some("Ab"));
    assert_eq!(m("(?i)[a-c]+", "AbC"), Some("AbC"));
    assert_eq!(m("(?i)[^a]", "Ab"), Some("b"));
    assert_eq!(m("(?s).", "\n"), Some("\n"));
    assert_eq!(m(".", "\n"), None);
    assert_eq!(m("(?m)^b$", "a\nb\nc"), Some("b"));
    assert_eq!(m("^b$", "a\nb\nc"), None);
    assert_eq!(m("(?x) a b # comment\n c", "abc"), Some("abc"));
    assert_eq!(m("(?x)[ ]a", " a"), Some(" a"));
    let o = Options { case_insensitive: true, ..Options::default() };
    assert!(Regex::new("abc", &o).unwrap().captures_at("ABC", 0..3, &mut Budget::new(100)).unwrap().is_some());
}

#[test]
fn quoting_and_escapes() {
    assert_eq!(m(r"\Qa.b*\E+", "a.b**"), Some("a.b**"));
    assert_eq!(m(r"\Qa.b", "xa.b"), Some("a.b"), "\\Q runs to the end");
    assert_eq!(m(r"\t\n\r\e\f\a", "\t\n\r\x1b\x0c\x07"), Some("\t\n\r\x1b\x0c\x07"));
    assert_eq!(m(r"\x41\x{1F600}\o{101}\101|\cA", "\x01"), Some("\x01"));
    assert_eq!(m(r"\x41\x{1F600}\o{101}\101", "A😀AA"), Some("A😀AA"));
    assert_eq!(m(r"\.\*\\", r".*\"), Some(r".*\"));
    assert!(matches!(err(r"\q"), Error::Syntax { .. }));
    assert!(matches!(err(r"\"), Error::Syntax { .. }));
}

#[test]
fn classes() {
    assert_eq!(m(r"[a-c]+", "xxabcab"), Some("abcab"));
    assert_eq!(m(r"[^a-c]+", "abxyc"), Some("xy"));
    assert_eq!(m(r"[]a]+", "]a]"), Some("]a]"));
    assert_eq!(m(r"[a-]+", "a-a"), Some("a-a"));
    assert_eq!(m(r"[\d.]+", "v1.25"), Some("1.25"));
    assert_eq!(m(r"\d+", "x٣٤"), Some("٣٤"), "Unicode digits");
    assert_eq!(m(r"\w+", "café!"), Some("café"));
    assert_eq!(m(r"\W", "ab-"), Some("-"));
    assert_eq!(m(r"\s+", "a \u{A0}\tb"), Some(" \u{A0}\t"));
    assert_eq!(m(r"\h+", "a \t\u{3000}\nb"), Some(" \t\u{3000}"));
    assert_eq!(m(r"\v", "a\u{2028}"), Some("\u{2028}"));
    assert_eq!(m(r"\p{Greek}+", "abc λόγος"), Some("λόγος"));
    assert_eq!(m(r"\P{L}+", "ab12cd"), Some("12"));
    assert_eq!(m(r"\pL+", "12ab"), Some("ab"));
    assert_eq!(m(r"\p{^L}+", "ab12"), Some("12"));
    assert_eq!(m(r"[[:alpha:]]+", "1abc2"), Some("abc"));
    assert_eq!(m(r"[[:^digit:]]+", "12ab3"), Some("ab"));
    assert_eq!(m(r"[\p{Lu}\d]+", "abC1D"), Some("C1D"));
    assert_eq!(m(r"[\b]", "a\x08"), Some("\x08"));
    assert!(matches!(err(r"[z-a]"), Error::Syntax { .. }));
    assert!(matches!(err(r"[[:nope:]]"), Error::Syntax { .. }));
    assert!(matches!(err(r"\p{Nope}"), Error::Syntax { .. }));
    assert!(matches!(err(r"[abc"), Error::Syntax { .. }));
}

#[test]
fn newlines_dot_and_anchors() {
    let nl = ['\n', '\u{1D}'];
    let o = Options { newlines: &nl, multiline: true, ..Options::default() };
    let r = Regex::new("^b.$", &o).unwrap();
    let hay = "a\u{1D}b!\nc";
    let c = r.captures_at(hay, 0..hay.len(), &mut Budget::new(1_000)).unwrap().unwrap();
    assert_eq!(&hay[c.get(0).unwrap()], "b!");
    assert_eq!(m(r"\N+", "ab\ncd"), Some("ab"));
    assert_eq!(m(r"(?s)\N+", "ab\ncd"), Some("ab"));
    assert_eq!(m(r"\R", "a\r\nb"), Some("\r\n"));
    assert_eq!(m(r"\R\R", "\r\n"), None, "\\R is atomic over \\r\\n");
    assert_eq!(m(r"a$", "a\n"), Some("a"), "$ allows a final newline");
    assert_eq!(m(r"a\Z", "a\n"), Some("a"));
    assert_eq!(m(r"a\z", "a\n"), None);
    assert_eq!(m(r"\Aa", "ba"), None);
    assert_eq!(m(r"(?m)^", "a\n").map(str::len), Some(0));
    assert_eq!(caps(r"(?m)^$", "a\n"), None, "^ does not match after a final newline");
    assert_eq!(m(r"\bis\b", "this is"), Some("is"));
    assert_eq!(m(r"\Bis\b", "this is").map(|_| ()), Some(()));
    assert_eq!(m(r"\bé", "café é"), Some("é"));
}

#[test]
fn ranges() {
    let r = re(r"\bb\w*");
    let hay = "ab bc bd";
    let mut b = Budget::new(10_000);
    let c = r.captures_at(hay, 1..hay.len(), &mut b).unwrap().unwrap();
    assert_eq!(c.get(0), Some(3..5), "\\b sees the text before the range");
    assert_eq!(r.captures_at(hay, 6..7, &mut b).unwrap().unwrap().get(0), Some(6..7), "text after the range is cut off");
    assert_eq!(re("(?<=a)b").captures_at("ab", 1..2, &mut b).unwrap().unwrap().get(0), Some(1..2));
    assert_eq!(re("x*").captures_at("ab", 2..2, &mut b).unwrap().unwrap().get(0), Some(2..2));
    #[allow(clippy::reversed_empty_ranges)]
    let reversed = 2..1;
    assert_eq!(re("x*").captures_at("ab", reversed, &mut b), Ok(None));
    assert_eq!(re("x*").captures_at("é", 1..2, &mut b), Ok(None));
    assert_eq!(re("x*").captures_at("é", 0..1, &mut b), Ok(None));
    assert_eq!(re("x*").captures_at("ab", 0..9, &mut b), Ok(None));
}

#[test]
fn budget_is_shared() {
    let r = re("a");
    let mut b = Budget::new(50);
    let hay = "x".repeat(100);
    assert_eq!(r.captures_at(&hay, 0..100, &mut b), Err(Error::TooComplex));
    assert_eq!(b.remaining(), 0);
    let mut b = Budget::new(1_000);
    assert!(r.captures_at("xa", 0..2, &mut b).unwrap().is_some());
    let left = b.remaining();
    assert!(left < 1_000);
    assert!(r.captures_at("xa", 0..2, &mut b).unwrap().is_some());
    assert!(b.remaining() < left);
}

#[test]
fn unsupported_and_errors() {
    for p in [r"(*FAIL)", r"(?C1)", r"\C", r"\G", r"(?U)", r"(?J)", r"(?X)", r"\X", r"(?^)", r"[\Q]\E]"] {
        assert!(matches!(err(p), Error::Unsupported(_)), "{p}");
    }
    for (p, pos) in [("a)", 1), ("(a", 0), ("*a", 0), ("a{2,1}", 2), ("(?<n>a)(?<n>b)", 7), ("\\2(a)", 0), ("(?&x)", 0)] {
        match err(p) {
            Error::Syntax { pos: at, .. } => assert_eq!(at, pos, "{p}"),
            e => panic!("{p}: {e}"),
        }
    }
    assert_eq!(err("\\k<nope>"), Error::Syntax { pos: 0, msg: "reference to non-existent group name 'nope'".into() });
    assert!(Regex::new(&"a".repeat(1_001), &Options::default()).is_err());
}

#[test]
fn readme_claims() {
    assert_eq!(caps("(?:(a|))*", "a"), Some(vec![Some("a"), Some("")]), "empty last iteration keeps its captures");
    assert_eq!(m(r"(a|b)\g<1>\g'1'", "aba"), Some("aba"), "Oniguruma-style subroutine calls");
    assert_eq!(m(r"(x)?(?(-1)y|z)", "xy"), Some("xy"));
    assert_eq!(m(r"(x)?(?(-1)y|z)", "z"), Some("z"));
    assert_eq!(m(r"^(?<n>x)?(?(n)y|z)$", "xy"), Some("xy"), "bare name condition");
    assert_eq!(m(r"(\[(?:[^\[\]]|(?1))*\])(?(R1)!|\.)", "[[a]]."), Some("[[a]]."));
    assert_eq!(m(r"(?<b>\[(?:[^\[\]]|(?&b))*\])(?(R&b)!|\.)", "[a]."), Some("[a]."));
    assert_eq!(m(r"\011", "\t"), Some("\t"));
    assert_eq!(m(r"a(?#comment)b", "ab"), Some("ab"));
    assert_eq!(m(r"(?im-s)^A.", "x\na\n"), None);
    assert_eq!(m(r"(?im-s)^A.", "x\nab"), Some("ab"));
    assert_eq!(m(r"(?<=\Qa.\E)b", "a.b"), Some("b"));
}
