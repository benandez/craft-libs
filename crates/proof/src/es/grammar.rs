//! Spanish grammar checks with Spanish messages: the generic repeated-word and spacing checks,
//! and the opening question and exclamation marks Spanish writes before a question or an
//! exclamation (`¿Vienes?`, `¡Qué bien!`): a closing `?` or `!` whose sentence has no opening mark
//! is flagged, with the opening mark offered at the sentence's start, and so is a space after an
//! opening mark (`¿ Vienes?`). Either opening mark may pair with either closing one (`¡Qué
//! dices?`), and `(?)` and `(!)`, which mark doubt or surprise, stand alone. No other
//! Spanish-specific grammar, comma or sentence-capital rules are inferred.

use crate::{Issue, IssueKind, ProofLang, words};

fn issue(start: usize, end: usize, message: String, suggestions: Vec<String>, lang: ProofLang) -> Issue {
    Issue { start, end, kind: IssueKind::Grammar, message, suggestions, lang }
}

/// Grammar issues in a paragraph of Spanish text; `lang` is the variety (they explain the same in
/// every one).
pub fn check(text: &str, lang: ProofLang) -> Vec<Issue> {
    let mut issues = Vec::new();
    repeated_words(text, &words(text), lang, &mut issues);
    spaces(text, lang, &mut issues);
    opening_marks(text, lang, &mut issues);
    issues.sort_by_key(|i| (i.start, i.end));
    issues
}

fn repeated_words(text: &str, ws: &[(usize, usize)], lang: ProofLang, v: &mut Vec<Issue>) {
    for pair in ws.windows(2) {
        let [(a0, b0), (a1, b1)] = pair else { continue };
        let (Some(w0), Some(w1), Some(gap)) = (text.get(*a0..*b0), text.get(*a1..*b1), text.get(*b0..*a1)) else { continue };
        if !gap.is_empty() && gap.chars().all(char::is_whitespace) && w0.chars().all(char::is_alphabetic) && w0.to_lowercase() == w1.to_lowercase() {
            v.push(issue(*a0, *b1, format!("Palabra repetida: «{w1}»"), vec![w0.to_string()], lang));
        }
    }
}

fn spaces(text: &str, lang: ProofLang, v: &mut Vec<Issue>) {
    for (i, c) in text.char_indices() {
        // A space after an opening mark: `¿ Vienes?`.
        if matches!(c, '¿' | '¡')
            && text.get(i + c.len_utf8()..).is_some_and(|r| r.starts_with(' ') && r.trim_start().chars().next().is_some_and(char::is_alphanumeric))
        {
            v.push(issue(i, i + c.len_utf8() + 1, "Espacio después de un signo de apertura".into(), vec![c.to_string()], lang));
            continue;
        }
        if c != ' ' {
            continue;
        }
        let rest = text.get(i + 1..).unwrap_or("");
        let mut it = rest.chars();
        let Some(p) = it.next() else { continue };
        let after = it.next();
        // A ratio/score keeps the spaces in digit-space-colon-space-digit.
        let ratio = p == ':'
            && text.get(..i).and_then(|before| before.chars().next_back()).is_some_and(|c| c.is_ascii_digit())
            && after == Some(' ')
            && it.next().is_some_and(|c| c.is_ascii_digit());
        let flag = !ratio
            && match p {
                ',' => true,
                // `;)` and `;-)` are emoticons.
                ';' => !matches!(after, Some(')' | '(' | '-' | 'P' | 'D')),
                // ` ...` is an ellipsis, ` .5` a number.
                '.' => !matches!(after, Some('.')) && !after.is_some_and(|a| a.is_ascii_digit()),
                // ` :)`, ` :-)`, ` :D` and similar are emoticons; ` ?!` is still a stray space.
                ':' | '!' | '?' => after.is_none_or(|a| a.is_whitespace() || matches!(a, '"' | '”' | '’' | '»' | '!' | '?')),
                _ => false,
            };
        if flag {
            v.push(issue(i, i + 1 + p.len_utf8(), "Espacio antes de un signo de puntuación".into(), vec![p.to_string()], lang));
        }
    }
    for (i, _) in text.match_indices("  ") {
        if i > 0 && text.get(..i).is_some_and(|t| t.ends_with(' ')) {
            continue;
        }
        v.push(issue(i, i + 2, "Espacio doble".into(), vec![" ".into()], lang));
    }
}

/// Characters a question or exclamation can't reach back past: sentence ends, clause breaks,
/// line breaks, and the start of a quotation or parenthesis.
fn is_boundary(c: char) -> bool {
    matches!(c, '.' | '?' | '!' | '…' | ':' | ';' | '\n' | '\r' | '\t' | '«' | '“' | '"' | '(' | '[' | '—')
}

/// A closing `?` or `!` whose sentence has no opening `¿` or `¡`.
fn opening_marks(text: &str, lang: ProofLang, v: &mut Vec<Issue>) {
    for (i, c) in text.char_indices() {
        let (opener, message) = match c {
            '?' => ('¿', "Falta el signo de interrogación de apertura (¿)"),
            '!' => ('¡', "Falta el signo de exclamación de apertura (¡)"),
            _ => continue,
        };
        let before = text.get(..i).unwrap_or("");
        let prev = before.chars().next_back();
        let next = text.get(i + 1..).and_then(|r| r.chars().next());
        // `?!` and `!!!` are handled at their first mark; `(?)` stands alone; `?q=` and `a!b`
        // aren't sentence ends.
        if matches!(prev, Some('?' | '!')) || (prev == Some('(') && next == Some(')')) || next.is_some_and(char::is_alphanumeric) {
            continue;
        }
        let from = before.char_indices().rev().find(|&(_, b)| is_boundary(b) || matches!(b, '¿' | '¡'));
        if from.is_some_and(|(_, b)| matches!(b, '¿' | '¡')) {
            continue;
        }
        let start = from.map_or(0, |(j, b)| j + b.len_utf8());
        let Some(sentence) = text.get(start..i) else { continue };
        let lead = sentence.len() - sentence.trim_start().len();
        let start = start + lead;
        let Some(sentence) = text.get(start..i + 1) else { continue };
        if !sentence.chars().any(char::is_alphabetic) {
            continue;
        }
        v.push(issue(start, i + 1, message.into(), vec![format!("{opener}{sentence}")], lang));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<(String, String)> {
        check(text, ProofLang::Es).into_iter().map(|i| (text[i.start..i.end].to_string(), i.message)).collect()
    }

    #[test]
    fn repeated_words_and_spaces_have_spanish_messages() {
        let text = "Es es la casa ,  dijo.";
        let issues = check(text, ProofLang::EsMx);
        assert_eq!(issues.len(), 3, "{issues:?}");
        assert_eq!(issues[0].message, "Palabra repetida: «es»");
        assert_eq!(issues[0].suggestions, ["Es"]);
        assert!(issues.iter().any(|i| i.message == "Espacio antes de un signo de puntuación" && &text[i.start..i.end] == " ,"));
        assert!(issues.iter().any(|i| i.message == "Espacio doble"));
        assert!(issues.iter().all(|i| i.lang == ProofLang::EsMx && i.kind == IssueKind::Grammar));
        for text in ["La casa. Es grande", "Sr. García", "No, no lo sé", "Hola :) número .5 y puntos ...", "3 : 1", "teórico-práctico"] {
            assert!(check(text, ProofLang::Es).is_empty(), "{text}: {:?}", check(text, ProofLang::Es));
        }
    }

    #[test]
    fn questions_and_exclamations_need_their_opening_mark() {
        assert_eq!(messages("Vienes mañana?"), [("Vienes mañana?".into(), "Falta el signo de interrogación de apertura (¿)".into())]);
        assert_eq!(check("Vienes mañana?", ProofLang::Es)[0].suggestions, ["¿Vienes mañana?"]);
        assert_eq!(messages("Llegó. Qué bien!"), [("Qué bien!".into(), "Falta el signo de exclamación de apertura (¡)".into())]);
        assert_eq!(messages("Le preguntó: vienes?")[0].0, "vienes?");
        assert_eq!(messages("Dijo «vienes?»")[0].0, "vienes?");
        assert_eq!(messages("Hola!!!").len(), 1);
        assert_eq!(messages("Qué?!").len(), 1);
        for text in [
            "¿Vienes mañana?",
            "¡Qué bien!",
            "Si vienes, ¿me llamas?",
            "¡Qué dices?",
            "¿Qué?! ¡No!",
            "Nació en 1950 (?) en Montevideo.",
            "Lo dijo (!) en público.",
            "Ver https://example.com/?q=1 y a!b",
            "¿Qué? ¿Cómo?",
            "2 + 2 = ?",
            "",
            "?",
        ] {
            assert!(messages(text).iter().all(|(_, m)| !m.starts_with("Falta")), "{text}: {:?}", messages(text));
        }
    }

    #[test]
    fn a_space_after_an_opening_mark_is_flagged() {
        let text = "¿ Vienes? ¡ Hola!";
        let issues = check(text, ProofLang::EsUy);
        let spaced: Vec<&str> =
            issues.iter().filter(|i| i.message == "Espacio después de un signo de apertura").map(|i| &text[i.start..i.end]).collect();
        assert_eq!(spaced, ["¿ ", "¡ "], "{issues:?}");
        assert!(issues.iter().all(|i| !i.message.starts_with("Falta")), "{issues:?}");
        assert!(check("¿ ", ProofLang::Es).is_empty());
    }

    proptest::proptest! {
        #[test]
        fn never_panics_and_ranges_are_valid(s in "\\PC{0,80}|[¿¡?!a-z .,(«»):]{0,40}") {
            for issue in check(&s, ProofLang::Es) {
                proptest::prop_assert!(issue.start <= issue.end && issue.end <= s.len());
                proptest::prop_assert!(s.is_char_boundary(issue.start) && s.is_char_boundary(issue.end));
            }
        }
    }
}
