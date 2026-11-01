//! Installed Language packs: a language's spelling dictionary, hyphenation patterns and licence
//! texts, downloaded and unpacked by the app's font store (craft-libs `craft-font-store`, item
//! kind `language-pack`) and read here at runtime.
//!
//! The packs folder ([`set_packs_dir`]) holds one folder per pack, named by its locale (lowercase
//! BCP 47: `nl`, `pt-br`, `sr-latn`), laid out like the pack archive's root:
//!
//! ```text
//! <packs folder>/<locale>/pack.toml        locale, name, licence, class, sources
//!                        /spelling.aff     Hunspell affix file      (optional)
//!                        /spelling.dic     Hunspell word list       (optional)
//!                        /hyphenation.pat  TeX/Liang patterns       (optional)
//!                        /LICENCES/<file>  the upstream licence texts
//! ```
//!
//! Lookup order for a language's spelling: the dictionaries folder, then an installed pack, then
//! the system's Hunspell dictionaries, then (English only) the core word list built into the
//! crate. For hyphenation: an installed pack, then the patterns embedded at build time.
//!
//! Every file is size-capped and read only when it is a regular file (never through a symbolic
//! link); a pack whose `pack.toml` is missing, unreadable or names a path outside the pack is
//! skipped and reported by [`load_errors`]. Nothing here panics on a hostile pack.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::lang::ProofLang;
use crate::patterns::Patterns;

/// The base name of a pack's Hunspell pair (`spelling.aff`, `spelling.dic`).
pub(crate) const SPELLING: &str = "spelling";
/// A pack's hyphenation patterns.
const HYPHENATION: &str = "hyphenation.pat";
/// A pack's description.
const PACK_TOML: &str = "pack.toml";
/// A pack's licence texts.
const LICENCES: &str = "LICENCES";
/// Largest `pack.toml` read.
const MAX_PACK_TOML: u64 = 1 << 20;
/// Largest licence file read.
const MAX_LICENCE: u64 = 4 << 20;
/// Most licence files read from one pack.
const MAX_LICENCE_FILES: usize = 64;
/// Most packs read from the packs folder.
const MAX_PACKS: usize = 1024;
/// Longest locale tag.
const MAX_LOCALE: usize = 35;

/// An installed Language pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPack {
    /// Lowercase BCP 47 tag (`nl`, `pt-br`), also the pack's folder name.
    pub locale: String,
    /// The language's own name (`Nederlands`); the locale when `pack.toml` gives none.
    pub name: String,
    /// SPDX licence expression of the pack's data.
    pub licence: String,
    /// The pack's folder.
    pub path: PathBuf,
}

/// What a scan of the packs folder found, with what each pack holds.
#[derive(Default)]
struct Scan {
    packs: Vec<(InstalledPack, Holds)>,
    /// (locale or folder name, reason) of each pack that was skipped.
    errors: Vec<(String, String)>,
}

#[derive(Clone, Copy, Default)]
struct Holds {
    spelling: bool,
    hyphenation: bool,
}

struct State {
    dir: Option<PathBuf>,
    scan: Option<Arc<Scan>>,
    /// Each language's pack patterns once looked up (`None`: no pack has them).
    hyphenation: Vec<(ProofLang, Option<Arc<Patterns>>)>,
    /// (locale, reason) of pack hyphenation files that failed to load.
    hyphenation_errors: Vec<(String, String)>,
    /// Counts changes, so a scan or load that started before one is not kept.
    changes: u64,
}

static STATE: RwLock<State> = RwLock::new(State { dir: None, scan: None, hyphenation: Vec::new(), hyphenation_errors: Vec::new(), changes: 0 });

fn read() -> RwLockReadGuard<'static, State> {
    STATE.read().unwrap_or_else(PoisonError::into_inner)
}

fn write() -> RwLockWriteGuard<'static, State> {
    STATE.write().unwrap_or_else(PoisonError::into_inner)
}

/// Use `dir` as the installed-packs folder (`<font-store folder>/language-packs`); `None`: no
/// packs. Every dictionary loads again and text is checked again
/// ([`crate::dictionary_generation`] changes), so call it again after a pack is installed or
/// removed (or call [`reload`]).
pub fn set_packs_dir(dir: Option<PathBuf>) {
    {
        let mut s = write();
        s.dir = dir;
        forget(&mut s);
    }
    crate::reload_dictionaries();
}

/// Read the packs folder again (a pack was installed or removed), as [`set_packs_dir`] with the
/// same folder.
pub fn reload() {
    forget(&mut write());
    crate::reload_dictionaries();
}

fn forget(s: &mut State) {
    s.scan = None;
    s.hyphenation.clear();
    s.hyphenation_errors.clear();
    s.changes = s.changes.wrapping_add(1);
}

/// The installed-packs folder set with [`set_packs_dir`].
pub fn packs_dir() -> Option<PathBuf> {
    read().dir.clone()
}

/// The installed packs, by locale.
pub fn installed() -> Vec<InstalledPack> {
    scan().packs.iter().map(|(p, _)| p.clone()).collect()
}

/// Why each pack that couldn't be used was skipped or failed to load, as (locale or folder name,
/// reason): a missing or unreadable `pack.toml`, a path outside the pack, a hyphenation file that
/// isn't UTF-8 or is too large. A spelling dictionary that fails to load is reported by
/// [`crate::dictionary_load_error`] like any other.
pub fn load_errors() -> Vec<(String, String)> {
    let mut out = scan().errors.clone();
    out.extend(read().hyphenation_errors.iter().cloned());
    out
}

/// The licence texts of the Language data in use, as (title, full text): the English data
/// embedded at build time, then each installed pack's `LICENCES/` files (About › Language data).
pub fn licence_texts() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if !crate::data::EN_US_DIC.is_empty() {
        out.push(("English word list and hyphenation (Moby Hyphenator II)".into(), MOBY_NOTICE.into()));
    }
    out.push(("English core word list".into(), CORE_LIST_NOTICE.into()));
    out.extend(crate::data::EMBEDDED_LICENCES.iter().map(|(t, x)| ((*t).to_string(), (*x).to_string())));
    for (p, _) in &scan().packs {
        for (file, text) in licence_files(&p.path) {
            out.push((format!("{} ({}): {file}", p.name, p.locale), text));
        }
    }
    out
}

/// The notice of the embedded Moby word list (craft-i18n `language-data/en-us/en-us.dic`).
const MOBY_NOTICE: &str = "The Moby Hyphenator II word list by Grady Ward (Project Gutenberg eBook #3204) is in the public domain. \
The English hyphenation patterns were trained from it for the Craft apps and are distributed with them under MIT OR Apache-2.0.";

/// The notice of the core word list built into the crate (`data/en-core.txt`).
const CORE_LIST_NOTICE: &str = "The core English word list built into craft-proof is taken from the Moby Hyphenator II word list by Grady Ward \
(public domain), ranked by public-domain Project Gutenberg books. It is in the public domain.";

/// The locales whose Language data is embedded in this build (`en-us`…, and in a Language build
/// the ones it names; `CRAFT_PROOF_EMBED`).
pub fn embedded_locales() -> &'static [&'static str] {
    crate::data::EMBEDDED_LOCALES
}

/// The scan of the packs folder, made on first use after each change.
fn scan() -> Arc<Scan> {
    let (dir, changes) = {
        let s = read();
        if let Some(scan) = &s.scan {
            return scan.clone();
        }
        (s.dir.clone(), s.changes)
    };
    let found = Arc::new(dir.as_deref().map(scan_dir).unwrap_or_default());
    let mut s = write();
    if s.changes != changes {
        // The folder changed meanwhile: this scan serves this call only.
        return found;
    }
    s.scan.get_or_insert(found).clone()
}

/// Read every pack in `dir` (sorted by folder name, at most [`MAX_PACKS`]).
fn scan_dir(dir: &Path) -> Scan {
    let mut scan = Scan::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            // No folder yet is no packs, not an error.
            if e.kind() != std::io::ErrorKind::NotFound {
                scan.errors.push((dir.display().to_string(), e.to_string()));
            }
            return scan;
        }
    };
    let mut names: Vec<String> = entries.filter_map(Result::ok).filter_map(|e| e.file_name().into_string().ok()).take(MAX_PACKS * 2).collect();
    names.sort();
    for name in names.into_iter().filter(|n| !n.starts_with('.')).take(MAX_PACKS) {
        match read_pack(&dir.join(&name), &name) {
            Ok(found) => scan.packs.push(found),
            Err(e) => {
                log::warn!("Language pack {name}: {e}");
                scan.errors.push((name, e));
            }
        }
    }
    scan
}

/// One pack folder, named `folder`.
fn read_pack(path: &Path, folder: &str) -> Result<(InstalledPack, Holds), String> {
    if !is_locale(folder) {
        return Err("the folder name isn't a lowercase language tag".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("not a folder".into());
    }
    let toml = path.join(PACK_TOML);
    if !is_regular_file(&toml) {
        return Err("no pack.toml".into());
    }
    let text = String::from_utf8(read_capped(&toml, MAX_PACK_TOML)?).map_err(|_| "pack.toml: not UTF-8".to_string())?;
    let info = parse_pack_toml(&text)?;
    if info.locale != folder {
        return Err(format!("pack.toml names locale {:?}, not {folder:?}", info.locale));
    }
    let holds = Holds {
        spelling: is_regular_file(&path.join(format!("{SPELLING}.aff"))) && is_regular_file(&path.join(format!("{SPELLING}.dic"))),
        hyphenation: is_regular_file(&path.join(HYPHENATION)),
    };
    let name = info.name.unwrap_or_else(|| info.locale.clone());
    Ok((InstalledPack { locale: info.locale, name, licence: info.licence, path: path.to_path_buf() }, holds))
}

/// What `pack.toml` says.
struct PackInfo {
    locale: String,
    name: Option<String>,
    licence: String,
}

/// Parse `pack.toml`: `locale` and `licence` are required strings, `name` optional; anything else
/// is kept for people, but every value under a key that names a file (`path`, `file`, `files`,
/// or ending in `_path`, `_file`, `_files`), at any depth, must be a plain relative path inside
/// the pack.
fn parse_pack_toml(text: &str) -> Result<PackInfo, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| format!("pack.toml: {}", e.message()))?;
    check_paths(&toml::Value::Table(table.clone()), None, 0)?;
    let string = |key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    let locale = string("locale").ok_or("pack.toml: no locale")?;
    if !is_locale(&locale) {
        return Err(format!("pack.toml: {locale:?} isn't a lowercase language tag"));
    }
    let licence = string("licence").or_else(|| string("license")).filter(|l| !l.trim().is_empty()).ok_or("pack.toml: no licence")?;
    let name = string("name").filter(|n| !n.trim().is_empty() && n.chars().count() <= 200 && !n.chars().any(char::is_control));
    Ok(PackInfo { locale, name, licence: licence.chars().take(200).collect() })
}

/// Deepest nesting of tables and arrays checked in `pack.toml` (deeper is refused).
const MAX_TOML_DEPTH: usize = 16;

/// Every string under a key that names a file is a [`safe_relative`] path.
fn check_paths(v: &toml::Value, key: Option<&str>, depth: usize) -> Result<(), String> {
    if depth > MAX_TOML_DEPTH {
        return Err("pack.toml: nested too deeply".into());
    }
    let names_file = key.is_some_and(|k| {
        let k = k.to_ascii_lowercase();
        ["path", "file", "files"].contains(&k.as_str()) || k.ends_with("_path") || k.ends_with("_file") || k.ends_with("_files")
    });
    match v {
        toml::Value::String(s) if names_file && !safe_relative(s) => {
            Err(format!("pack.toml: {:?} = {s:?} points outside the pack", key.unwrap_or_default()))
        }
        toml::Value::Table(t) => t.iter().try_for_each(|(k, v)| check_paths(v, Some(k), depth + 1)),
        toml::Value::Array(a) => a.iter().try_for_each(|v| check_paths(v, key, depth + 1)),
        _ => Ok(()),
    }
}

/// A relative path that stays inside the folder it is read from: no root, drive, `..`, `.`,
/// backslash, empty part or control character.
fn safe_relative(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && !p.contains(':')
        && !p.chars().any(char::is_control)
        && p.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A lowercase BCP 47 tag as packs are named: ASCII lowercase letters and digits in parts of 1 to
/// 8 joined by `-`, at most [`MAX_LOCALE`] characters.
fn is_locale(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_LOCALE
        && s.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// A regular file, not a symbolic link (a pack never reads through one).
fn is_regular_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
}

/// A pack file: a regular file (not a symbolic link) of at most `cap` bytes.
fn read_capped(p: &Path, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if !is_regular_file(p) {
        return Err(format!("{}: not a regular file", p.display()));
    }
    let err = |e: std::io::Error| format!("{}: {e}", p.display());
    let f = std::fs::File::open(p).map_err(err)?;
    let mut buf = Vec::new();
    f.take(cap.saturating_add(1)).read_to_end(&mut buf).map_err(err)?;
    if buf.len() as u64 > cap {
        return Err(format!("{}: too large (over {cap} bytes)", p.display()));
    }
    Ok(buf)
}

/// The licence texts in a pack's `LICENCES/` folder, by file name.
fn licence_files(pack: &Path) -> Vec<(String, String)> {
    let dir = pack.join(LICENCES);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .take(MAX_LICENCE_FILES)
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let bytes = read_capped(&dir.join(&n), MAX_LICENCE).ok()?;
            Some((n, String::from_utf8_lossy(&bytes).into_owned()))
        })
        .collect()
}

/// Lowercase pack locales for Hunspell dictionary names: `pt_BR` gives `pt-br`, then `pt`.
fn locales_for(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |l: String| {
        if is_locale(&l) && !out.contains(&l) {
            out.push(l);
        }
    };
    for n in names {
        push(n.to_ascii_lowercase().replace('_', "-"));
    }
    // The primary language alone, unless a script is named (`sr_Latn` is never plain `sr`).
    for n in names.iter().filter(|n| !has_script(n)) {
        push(n.split(['_', '-']).next().unwrap_or_default().to_ascii_lowercase());
    }
    out
}

/// Does a tag or file name name a script (a four-letter subtag after the language: `sr-Latn`)?
fn has_script(tag: &str) -> bool {
    tag.split(['_', '-']).skip(1).any(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// The installed packs to read for `locales`, in that order, then any other pack of their
/// primary languages (`pt` → `pt-br`, `pt-pt`), keeping those that `has` the data.
fn packs_for(locales: &[String], has: impl Fn(&Holds) -> bool) -> Vec<PathBuf> {
    let scan = scan();
    let usable = || scan.packs.iter().filter(|(_, h)| has(h));
    let mut out: Vec<PathBuf> = Vec::new();
    for l in locales {
        if let Some((p, _)) = usable().find(|(p, _)| &p.locale == l) {
            out.push(p.path.clone());
        }
    }
    // Then another region's pack of the same language (`pt` → `pt-br`), never another script's.
    let primaries: Vec<&str> = locales.iter().filter(|l| !has_script(l)).map(|l| l.split('-').next().unwrap_or_default()).collect();
    for (p, _) in usable() {
        let primary = p.locale.split('-').next().unwrap_or_default();
        if primaries.contains(&primary) && !has_script(&p.locale) && !out.contains(&p.path) {
            out.push(p.path.clone());
        }
    }
    out
}

/// The folders of the installed packs with a spelling dictionary for the Hunspell dictionary
/// `names` (`pl_PL`, `pl`), best first ([`crate::hunspell::find`] reads them).
pub(crate) fn spelling_dirs(names: &[&str]) -> Vec<PathBuf> {
    if read().dir.is_none() {
        return Vec::new();
    }
    packs_for(&locales_for(names), |h| h.spelling)
}

/// The pack locales whose patterns hyphenate `lang`, best first; none for English (embedded)
/// and languages without proofing.
fn hyphenation_locales(lang: ProofLang) -> Vec<String> {
    let names: &[&str] = match lang {
        ProofLang::Pl => &["pl"],
        ProofLang::Bg => &["bg"],
        ProofLang::Pt => &["pt"],
        ProofLang::It => &["it"],
        ProofLang::Hr => &["hr"],
        ProofLang::Tr => &["tr"],
        ProofLang::Ru => &["ru"],
        ProofLang::Cs => &["cs"],
        ProofLang::Id => &["id"],
        ProofLang::Es => &["es-es", "es"],
        ProofLang::EsMx => &["es-mx", "es-419", "es"],
        ProofLang::Es419 => &["es-419", "es"],
        ProofLang::EsUy => &["es-uy", "es-419", "es"],
        ProofLang::De => &["de", "de-1996", "de-de"],
        ProofLang::Pack(l) => return crate::pack_lang::hyphenation_locales(l),
        ProofLang::En | ProofLang::EnGb | ProofLang::EnCa | ProofLang::Other => &[],
    };
    names.iter().map(|n| (*n).to_string()).collect()
}

/// The hyphenation patterns of an installed pack for `lang`, if there is one (read on first use
/// after each change). A pack file that fails to load is reported by [`load_errors`] and the next
/// pack (or the embedded patterns) is used.
pub(crate) fn hyphenation(lang: ProofLang) -> Option<Arc<Patterns>> {
    let changes = {
        let s = read();
        s.dir.as_ref()?;
        if let Some((_, p)) = s.hyphenation.iter().find(|(l, _)| *l == lang) {
            return p.clone();
        }
        s.changes
    };
    let mut errors = Vec::new();
    let mut found = None;
    for dir in packs_for(&hyphenation_locales(lang), |h| h.hyphenation) {
        match load_patterns(&dir.join(HYPHENATION)) {
            Ok(p) => {
                found = Some(Arc::new(p));
                break;
            }
            Err(e) => {
                let locale = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                log::warn!("Language pack {locale}: {e}");
                errors.push((locale, e));
            }
        }
    }
    let mut s = write();
    if s.changes == changes && !s.hyphenation.iter().any(|(l, _)| *l == lang) {
        s.hyphenation.push((lang, found.clone()));
        for e in errors {
            if !s.hyphenation_errors.contains(&e) {
                s.hyphenation_errors.push(e);
            }
        }
    }
    found
}

/// A pack's `hyphenation.pat` (UTF-8 TeX or plain Liang patterns, at most the dictionary cap).
fn load_patterns(path: &Path) -> Result<Patterns, String> {
    let bytes = read_capped(path, crate::hunspell::MAX_FILE)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8", path.display()))?;
    let p = Patterns::parse(&text);
    if p.is_empty() {
        return Err(format!("{}: no patterns", path.display()));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_are_lowercase_tags() {
        for ok in ["nl", "pt-br", "sr-latn", "es-419", "de-1996", "ca-valencia"] {
            assert!(is_locale(ok), "{ok}");
        }
        for bad in ["", "NL", "pt_BR", "..", "a/b", "-nl", "nl-", "nl--x", "abcdefghi", "x\u{0}", "é", &"a-".repeat(20)] {
            assert!(!is_locale(bad), "{bad:?}");
        }
    }

    #[test]
    fn dictionary_names_map_to_pack_locales() {
        assert_eq!(locales_for(&["pt_BR", "pt-BR", "pt"]), ["pt-br", "pt"]);
        assert_eq!(locales_for(&["es_419", "es"]), ["es-419", "es"]);
        assert_eq!(locales_for(&["../x", "a b"]), Vec::<String>::new());
    }

    #[test]
    fn pack_toml_paths_stay_inside_the_pack() {
        let ok = "locale = \"nl\"\nlicence = \"BSD-3-Clause\"\nname = \"Nederlands\"\nsources = [\"https://example.org/nl.aff\"]\n[[file]]\npath = \"LICENCES/COPYING\"\n";
        let info = parse_pack_toml(ok).unwrap();
        assert_eq!((info.locale.as_str(), info.name.as_deref(), info.licence.as_str()), ("nl", Some("Nederlands"), "BSD-3-Clause"));
        for bad in ["../../etc/passwd", "/etc/passwd", "C:/x", "LICENCES/../../x", "a\\b", "", "./x"] {
            let t = format!("locale = \"nl\"\nlicence = \"MIT\"\n[[file]]\npath = {bad:?}\n");
            assert!(parse_pack_toml(&t).is_err(), "{bad:?}");
            let t = format!("locale = \"nl\"\nlicence = \"MIT\"\nlicence_files = [{bad:?}]\n");
            assert!(parse_pack_toml(&t).is_err(), "{bad:?}");
        }
        for bad in [
            "licence = \"MIT\"\n",
            "locale = \"nl\"\n",
            "locale = \"../nl\"\nlicence = \"MIT\"\n",
            "locale = \"nl\"\nlicence = \"MIT\"\nlocale = \"x\"",
            "\u{0}",
        ] {
            assert!(parse_pack_toml(bad).is_err(), "{bad:?}");
        }
        let deep = format!("locale = \"nl\"\nlicence = \"MIT\"\nx = {}{}\n", "[".repeat(40), "]".repeat(40));
        assert!(parse_pack_toml(&deep).is_err());
    }

    /// Review of #485: our own word supplements (`ru-extra.txt`, MIT) stay embedded in the
    /// default build; they aren't upstream data and don't make a locale "embedded".
    #[test]
    fn our_supplements_are_embedded_by_default() {
        if cfg!(craft_proof_data) {
            assert!(!crate::data::EXTRA_RU.is_empty(), "ru-extra.txt is embedded");
        }
        let embed = option_env!("CRAFT_PROOF_EMBED").unwrap_or_default();
        if embed.is_empty() {
            assert!(!embedded_locales().contains(&"ru"));
        }
    }

    #[test]
    fn embedded_locales_are_english_unless_a_language_build_names_more() {
        let embed = option_env!("CRAFT_PROOF_EMBED").unwrap_or_default();
        for l in embedded_locales() {
            assert!(l.starts_with("en-") || embed.split(',').any(|e| e.trim() == *l || e.trim() == "all"), "{l} is embedded");
        }
    }
}
