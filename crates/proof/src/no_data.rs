//! A build with no Language data embedded (no `CRAFT_I18N_DIR`): every public entry point still
//! works and never panics. English spelling marks nothing and suggests nothing, hyphenation that
//! needs the data is off, and grammar (code, not data) still works.

use crate::hyphen::{self, Limits, Source};
use crate::{Options, ProofLang, dict, en_variants, patterns};

const LANGS: [ProofLang; 18] = [
    ProofLang::En,
    ProofLang::EnGb,
    ProofLang::EnCa,
    ProofLang::Pl,
    ProofLang::Bg,
    ProofLang::Pt,
    ProofLang::It,
    ProofLang::Hr,
    ProofLang::Tr,
    ProofLang::Ru,
    ProofLang::Cs,
    ProofLang::Id,
    ProofLang::Es,
    ProofLang::EsMx,
    ProofLang::Es419,
    ProofLang::EsUy,
    ProofLang::De,
    ProofLang::Other,
];

/// Words and text that exercise each entry point, hostile ones included.
const SAMPLES: [&str; 10] = [
    "",
    "teh colour of hyphenation",
    "Ünïcödé — ‘quotes’ don't",
    "a a  apple .Next sentence",
    "https://example.org a@b.org",
    "zażółć gęślą jaźń",
    "Привет, мир — кто-то",
    "¿Qué tal? más",
    "\u{0}\u{FFFF}\u{10FFFF}",
    "supercalifragilisticexpialidociousxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
];

#[test]
fn nothing_is_embedded() {
    assert!(crate::data::EN_US_DIC.is_empty());
    assert!(dict::Dictionary::en_us().is_empty());
    for p in [patterns::Patterns::en_us(), patterns::Patterns::pl(), patterns::Patterns::pt(), patterns::Patterns::it(), patterns::Patterns::de()] {
        assert!(p.is_empty());
        assert_eq!(p.exception_count(), 0);
    }
}

#[test]
fn english_spelling_marks_nothing_and_suggests_nothing() {
    for w in ["teh", "colour", "color", "zxqv", "desktop"] {
        assert!(crate::is_correct(w), "{w}");
        assert!(crate::is_correct_with(w, &Options::default()), "{w}");
        assert!(crate::is_correct_for(w, Some("en-GB")), "{w}");
        for l in [ProofLang::En, ProofLang::EnGb, ProofLang::EnCa] {
            assert!(crate::is_correct_in(w, l), "{w} {l:?}");
            assert!(crate::suggest_in(w, 5, l).is_empty(), "{w} {l:?}");
        }
        assert!(crate::suggest(w, 5).is_empty(), "{w}");
        assert_eq!(en_variants::preferred(w, ProofLang::En), None);
        assert!(!en_variants::is_foreign(w, ProofLang::EnGb));
    }
    assert!(crate::check_spelling("Teh colour of zxqv").is_empty());
    assert!(crate::check_spelling_in("Teh colour", ProofLang::EnGb).is_empty());
}

#[test]
fn grammar_still_works() {
    assert!(!crate::check_grammar("This is is wrong.").is_empty());
}

#[test]
fn hyphenation_is_off_where_it_needs_data() {
    let lim = Limits::default();
    for (w, l) in [
        ("hyphenation", ProofLang::En),
        ("hyphenation", ProofLang::Cs),
        ("przedsiębiorstwo", ProofLang::Pl),
        ("computador", ProofLang::Pt),
        ("calcolatore", ProofLang::It),
        ("Silbentrennung", ProofLang::De),
    ] {
        assert_eq!(hyphen::word_breaks_in(w, l), (vec![], Source::None), "{w} {l:?}");
        assert!(hyphen::hyphen_points_in(w, &lim, l).is_empty(), "{w} {l:?}");
        assert_eq!(hyphen::hyphenate_word_in(w, &lim, l), w);
    }
    assert!(hyphen::hyphen_points("hyphenation", &lim).is_empty());
    assert_eq!(hyphen::hyphenate_word("hyphenation", &lim), "hyphenation");
    assert_eq!(hyphen::word_breaks("hyphenation").1, Source::None);
    // Rules are code, not data: Bulgarian still hyphenates.
    assert!(!hyphen::word_breaks_in("книжарница", ProofLang::Bg).0.is_empty());
}

/// Every public entry point, in every language, on every sample: no panic.
#[test]
fn every_public_function_runs_without_data() {
    let lim = Limits { min_word: 0, after_first: 0, before_last: 0, capitalized: true };
    let opts = Options { ignore_uppercase: false, ignore_numbers: false, ignore_internet: false };
    for text in SAMPLES {
        let len = text.len();
        let _ = crate::words(text);
        let _ = crate::check_spelling(text);
        let _ = crate::check_spelling_with(text, &opts);
        let _ = crate::check_grammar(text);
        let _ = crate::is_correct(text);
        let _ = crate::is_correct_with(text, &opts);
        let _ = crate::suggest(text, 5);
        let _ = crate::custom_word(text);
        let _ = crate::has_dictionary(Some(text));
        let _ = crate::missing_dictionary(Some(text));
        let _ = crate::unreadable_dictionary(Some(text));
        let _ = crate::is_correct_for(text, Some(text));
        let _ = hyphen::word_breaks(text);
        let _ = hyphen::hyphen_points(text, &lim);
        let _ = hyphen::hyphenate_word(text, &lim);
        let spans: Vec<(std::ops::Range<usize>, ProofLang)> = LANGS.iter().map(|&l| (0..len, l)).collect();
        let _ = crate::check_text(text, &spans);
        let _ = crate::check_text_with(text, &spans, &opts, true, true);
        // Out-of-range and reversed spans are hostile input too.
        #[allow(clippy::reversed_empty_ranges)]
        let _ = crate::check_text(text, &[(len + 5..len + 9, ProofLang::En), (3..1, ProofLang::Pl)]);
        for l in LANGS {
            let _ = crate::check_spelling_in(text, l);
            let _ = crate::check_grammar_in(text, l);
            let _ = crate::is_correct_in(text, l);
            let _ = crate::is_correct_in_with(text, l, &opts);
            let _ = crate::suggest_in(text, 5, l);
            let _ = crate::dictionary_load_error(l);
            let _ = hyphen::word_breaks_in(text, l);
            let _ = hyphen::hyphen_points_in(text, &lim, l);
            let _ = hyphen::hyphenate_word_in(text, &lim, l);
            let _ = en_variants::preferred(text, l);
            let _ = en_variants::is_foreign(text, l);
            let _ = en_variants::known_variant(text, l, |_| true);
        }
    }
    let _ = crate::installed_dictionaries();
    let _ = crate::dictionary_generation();
    let _ = crate::user_dictionary();
    let _ = crate::has_dictionary(None);
    let _ = crate::missing_dictionary(None);
    let _ = crate::unreadable_dictionary(None);
}
