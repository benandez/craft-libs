//! Proofing for any language by its locale ([`crate::ProofLang::Pack`]): its Hunspell dictionary (the
//! dictionaries folder, then an installed Language pack, then the system's), read by the shared
//! affix engine, with default suggestion tuning and no grammar rules.
//!
//! At most [`MAX_LOADED`] dictionaries stay loaded; the one used least recently goes first.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use crate::affix::AffixDict;
use crate::affix_suggest::Tuning;
use crate::lang::PackLocale;

/// Most locale dictionaries kept loaded at once.
pub(crate) const MAX_LOADED: usize = 8;

struct Loaded {
    locale: PackLocale,
    dict: Arc<AffixDict>,
    /// The `.dic` it came from (`None`: none found, the language is unchecked).
    file: Option<PathBuf>,
    used: u64,
}

struct Cache {
    loaded: Vec<Loaded>,
    clock: u64,
    /// Counts reloads, so a load that started before one is not kept.
    reloads: u64,
}

static CACHE: RwLock<Cache> = RwLock::new(Cache { loaded: Vec::new(), clock: 0, reloads: 0 });

/// Forget every loaded dictionary (the dictionaries folder or the packs changed).
pub(crate) fn reload() {
    let mut c = CACHE.write().unwrap_or_else(PoisonError::into_inner);
    c.loaded.clear();
    c.reloads = c.reloads.wrapping_add(1);
}

/// Hunspell file names for `locale`, best first: `fr-fr` → `fr_FR`, `fr-FR`, `fr`;
/// `sr-latn` → `sr_Latn`, `sr-Latn` (never plain `sr`, another script).
pub(crate) fn names(locale: PackLocale) -> Vec<String> {
    let parts: Vec<&str> = locale.as_str().split('-').collect();
    let cased: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| match (i, p.len()) {
            (0, _) => (*p).to_string(),
            (_, 2) => p.to_ascii_uppercase(),
            (_, 4) => {
                let mut c = p.chars();
                c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
            }
            _ => (*p).to_string(),
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: String| {
        if !out.contains(&n) {
            out.push(n);
        }
    };
    // Each shorter prefix too (`sr-latn-rs` → `sr-latn`), but not past a script subtag.
    let script = parts.iter().position(|p| p.len() == 4).unwrap_or(usize::MAX);
    for n in (1..=cased.len()).rev() {
        if n <= script && script != usize::MAX {
            break;
        }
        let prefix = cased.get(..n).unwrap_or_default();
        push(prefix.join("_"));
        push(prefix.join("-"));
    }
    if script != usize::MAX {
        let prefix = cased.get(..=script).unwrap_or_default();
        push(prefix.join("_"));
        push(prefix.join("-"));
    }
    out
}

/// `locale`'s dictionary, loaded on first use; an empty one when there is none.
fn get(locale: PackLocale) -> (Arc<AffixDict>, Option<PathBuf>) {
    let reloads = {
        let mut c = CACHE.write().unwrap_or_else(PoisonError::into_inner);
        c.clock = c.clock.wrapping_add(1);
        let now = c.clock;
        if let Some(l) = c.loaded.iter_mut().find(|l| l.locale == locale) {
            l.used = now;
            return (l.dict.clone(), l.file.clone());
        }
        c.reloads
    };
    let (dict, file) = load(locale);
    let mut c = CACHE.write().unwrap_or_else(PoisonError::into_inner);
    if c.reloads == reloads && !c.loaded.iter().any(|l| l.locale == locale) {
        if c.loaded.len() >= MAX_LOADED
            && let Some(oldest) = c.loaded.iter().enumerate().min_by_key(|(_, l)| l.used).map(|(i, _)| i)
        {
            c.loaded.swap_remove(oldest);
        }
        let used = c.clock;
        c.loaded.push(Loaded { locale, dict: dict.clone(), file: file.clone(), used });
    }
    (dict, file)
}

fn load(locale: PackLocale) -> (Arc<AffixDict>, Option<PathBuf>) {
    let names = names(locale);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let key = names.first().map_or("", String::as_str);
    let Some((aff, dic)) = crate::hunspell::find(&refs) else {
        crate::hunspell::clear_load_error(key);
        return (Arc::default(), None);
    };
    match crate::hunspell::note_load(key, &dic, crate::hunspell::load(&aff, &dic)) {
        Ok(d) => (Arc::new(d), Some(dic)),
        Err(e) => {
            log::warn!("{locale} spelling: {e}");
            (Arc::default(), None)
        }
    }
}

/// Is a dictionary for `locale` available (its spelling is checked)?
pub(crate) fn has_dictionary(locale: PackLocale) -> bool {
    !get(locale).0.is_empty()
}

/// The `.dic` file `locale`'s spelling is read from now.
pub(crate) fn dictionary_file(locale: PackLocale) -> Option<PathBuf> {
    let (d, file) = get(locale);
    file.filter(|_| !d.is_empty())
}

/// Why `locale`'s dictionary failed to load, if it did.
pub(crate) fn load_error(locale: PackLocale) -> Option<String> {
    crate::hunspell::load_error(names(locale).first()?).map(|(_, e)| e)
}

/// Is `word` right in `locale`? Every word is when there is no dictionary.
pub(crate) fn is_correct(word: &str, locale: PackLocale, opts: &crate::Options) -> bool {
    let (d, _) = get(locale);
    if d.is_empty() {
        return true;
    }
    let Some(w) = crate::spelling::word_to_check_in(&d, word, opts) else { return true };
    let known = |w: &str| d.check(w) || crate::in_user_dictionary(&w.to_lowercase());
    if w.contains('-') && !known(w) {
        return w.split('-').all(|p| p.chars().count() <= 1 || known(p));
    }
    known(w)
}

/// Suggestions for `word` in `locale`, best first, at most `max`.
pub(crate) fn suggest(word: &str, max: usize, locale: PackLocale) -> Vec<String> {
    let (d, _) = get(locale);
    if d.is_empty() {
        return Vec::new();
    }
    crate::affix_suggest::suggest_in(&d, word, max, &Tuning::DEFAULT)
}

/// The pack locales whose hyphenation patterns serve a [`crate::ProofLang::Pack`] language: the locale,
/// then each shorter prefix not past a script subtag (`fr-ca` → `fr`).
pub(crate) fn hyphenation_locales(locale: PackLocale) -> Vec<String> {
    names(locale).into_iter().filter(|n| !n.contains('_')).map(|n| n.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(t: &str) -> PackLocale {
        PackLocale::new(t).unwrap()
    }

    #[test]
    fn locales_and_file_names() {
        assert_eq!(l("nl-NL").as_str(), "nl-nl");
        assert_eq!(l("fr_FR.UTF-8").as_str(), "fr-fr");
        assert_eq!(l("ca-ES-valencia").as_str(), "ca-es-valencia");
        assert_eq!(l("sr-Latn-RS-x-private").as_str(), "sr-latn-rs");
        assert_eq!(l("zh-Hant-TW-somethinglong").as_str(), "zh-hant-tw");
        for bad in ["x-none", "i-klingon", "und", "zxx", "mul", "e", "-", "", "1x", "näh"] {
            assert_eq!(PackLocale::new(bad), None, "{bad}");
        }
        assert_eq!(names(l("nl")), ["nl"]);
        assert_eq!(names(l("fr-FR")), ["fr_FR", "fr-FR", "fr"]);
        assert_eq!(names(l("sr-Latn")), ["sr_Latn", "sr-Latn"]);
        assert_eq!(names(l("sr-Latn-RS")), ["sr_Latn_RS", "sr-Latn-RS", "sr_Latn", "sr-Latn"]);
        assert_eq!(hyphenation_locales(l("fr-CA")), ["fr-ca", "fr"]);
        assert_eq!(hyphenation_locales(l("sr-Latn")), ["sr-latn"]);
    }
}
