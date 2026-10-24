//! Random patterns from the subset both engines share must give the same match and groups as
//! `regex-automata` (leftmost-first). Haystacks have no newlines, so `$` means the same in both.

use craft_regex::{Budget, Options, Regex};
use proptest::prelude::*;

/// Patterns that can't match empty. Only these go under `*`, `+` and `{n,}`: on an empty
/// iteration PCRE2 keeps its captures and `regex-automata` drops them (see the README).
fn solid() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        Just("a".to_owned()),
        Just("b".to_owned()),
        Just("c".to_owned()),
        Just(" ".to_owned()),
        Just(".".to_owned()),
        Just("[ab]".to_owned()),
        Just("[^a]".to_owned()),
        Just("\\w".to_owned()),
    ];
    atom.prop_recursive(3, 12, 3, |inner| {
        let quant = prop_oneof![Just("+"), Just("{1,2}"), Just("{2}"), Just("{1,}")];
        prop_oneof![
            (inner.clone(), quant, any::<bool>()).prop_map(|(x, q, lazy)| format!("(?:{x}){q}{}", if lazy { "?" } else { "" })),
            inner.clone().prop_map(|x| format!("({x})")),
            (inner.clone(), inner.clone()).prop_map(|(x, y)| format!("{x}{y}")),
            (inner.clone(), inner).prop_map(|(x, y)| format!("{x}|{y}")),
        ]
    })
}

fn pattern() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        4 => solid(),
        1 => Just("\\b".to_owned()),
        1 => Just("^".to_owned()),
        1 => Just("$".to_owned()),
        1 => Just(String::new()),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        let quant = prop_oneof![Just("*"), Just("+"), Just("?"), Just("{0,2}"), Just("{0,}")];
        prop_oneof![
            (solid(), quant, any::<bool>()).prop_map(|(x, q, lazy)| format!("(?:{x}){q}{}", if lazy { "?" } else { "" })),
            (inner.clone(), any::<bool>()).prop_map(|(x, lazy)| format!("(?:{x})?{}", if lazy { "?" } else { "" })),
            inner.clone().prop_map(|x| format!("({x})")),
            (inner.clone(), inner.clone()).prop_map(|(x, y)| format!("{x}{y}")),
            (inner.clone(), inner).prop_map(|(x, y)| format!("{x}|{y}")),
        ]
    })
}

fn haystack() -> impl Strategy<Value = String> {
    proptest::collection::vec(prop_oneof![Just('a'), Just('b'), Just('c'), Just(' '), Just('é')], 0..12).prop_map(|v| v.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_000))]

    #[test]
    fn same_as_regex_automata(p in pattern(), hay in haystack(), start in 0usize..4) {
        let ours = Regex::new(&p, &Options { max_insts: 100_000, ..Options::default() }).unwrap();
        let theirs = regex_automata::meta::Regex::new(&p).unwrap();
        let start = (0..=start.min(hay.len())).rev().find(|&s| hay.is_char_boundary(s)).unwrap_or(0);
        let got = ours.captures_at(&hay, start..hay.len(), &mut Budget::new(10_000_000)).unwrap();
        let mut caps = theirs.create_captures();
        theirs.search_captures(&regex_automata::Input::new(&hay).range(start..), &mut caps);
        let want: Option<Vec<_>> = caps.is_match().then(|| {
            (0..caps.group_len()).map(|i| caps.get_group(i).map(|s| s.range())).collect()
        });
        let got: Option<Vec<_>> = got.map(|c| (0..c.len()).map(|i| c.get(i)).collect());
        prop_assert_eq!(got, want, "pattern {:?} on {:?} from {}", p, hay, start);
    }
}
