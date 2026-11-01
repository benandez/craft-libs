//! The store's second item kind, `language-pack`: a language's spelling dictionary, hyphenation
//! patterns and licence texts, published by storytold/craft-i18n as `<locale>.tar.zst` release
//! assets listed in an `index.json`, and read at runtime by `craft-proof` (`craft_proof::packs`).
//!
//! It reuses the store's folder, consent ([`FontStore::enable_network`]), HTTP client and sha256
//! checks: the app pins the index by URL and sha256 ([`PackIndexPin`]), each archive is checked
//! against the size and sha256 the index gives, then unpacked into `.staging/` and moved into
//! `language-packs/<locale>/` with one rename. Packs have no update checks of their own: a new
//! pin is a new index.
//!
//! ```text
//! <store>/language-packs/<locale>/   pack.toml, spelling.aff, spelling.dic, hyphenation.pat, LICENCES/…
//! <store>/language-packs.json        which packs are installed, from which index entry
//! <store>/indexes/language-packs.json  the last pack index fetched
//! ```
//!
//! **Developer override (testing only):** `CRAFT_LANGUAGE_PACK_INDEX=<url or path>` replaces the
//! pinned index URL with an `http://` URL on a loopback host (`127.0.0.1`, `localhost`, `[::1]`),
//! an `https://` URL or a local file path, and skips the index's sha256 pin. Archives are still
//! checked against the sha256 and size in that index, and may then also be loopback `http://`
//! URLs or local paths. It is off unless the variable is set, and logged as a warning when used.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::http::{Cancel, Client, Timeouts};
use crate::store::{Error, FontStore, INDEX_CACHE, Progress, Stage, io, read_limited};
use crate::verify::{VerifyError, sha256_hex};

/// The store's folder of installed Language packs.
pub const LANGUAGE_PACKS: &str = "language-packs";
/// The developer override of the pinned pack index (see the module docs). Testing only.
pub const LANGUAGE_PACK_INDEX_ENV: &str = "CRAFT_LANGUAGE_PACK_INDEX";
/// Largest pack index read.
pub const MAX_PACK_INDEX_BYTES: u64 = 4 << 20;
/// Largest pack archive downloaded.
pub const MAX_PACK_ARCHIVE_BYTES: u64 = 128 << 20;
/// Largest file unpacked from a pack (craft-proof's dictionary cap).
pub const MAX_PACK_FILE_BYTES: u64 = 64 << 20;
/// Largest total unpacked size of a pack.
pub const MAX_PACK_UNPACKED_BYTES: u64 = 256 << 20;
/// Most files and folders in a pack archive.
pub const MAX_PACK_ENTRIES: usize = 256;
/// Most packs in an index.
const MAX_PACKS: usize = 1024;
const INSTALLED: &str = "language-packs.json";
const INDEX_CACHE_NAME: &str = "language-packs.json";

/// The pack index an app trusts: `index.json` of a craft-i18n `language-packs-<n>` release, by
/// URL and sha256 (the app's Pin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackIndexPin {
    /// `https://github.com/storytold/craft-i18n/releases/download/language-packs-<n>/index.json`.
    pub url: &'static str,
    /// The sha256 of that file, lowercase hex.
    pub sha256: &'static str,
}

/// `index.json` of a pack release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackIndex {
    /// The format; 1.
    pub format: u32,
    /// The release (`language-packs-1`).
    pub release: String,
    /// The packs, by locale.
    pub packs: Vec<PackEntry>,
}

/// One pack of a [`PackIndex`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackEntry {
    /// Lowercase BCP 47 tag (`nl`, `pt-br`); the pack's folder name.
    pub locale: String,
    /// The language's own name (`Nederlands`).
    pub name: String,
    /// The craft-i18n commit it was built from.
    #[serde(default)]
    pub version: String,
    /// The archive (`<locale>.tar.zst`).
    pub url: String,
    /// The archive's sha256, lowercase hex.
    pub sha256: String,
    /// The archive's size in bytes.
    pub size: u64,
    /// SPDX licence expression.
    pub licence: String,
    /// The licence class: `Embeddable` or `Download-only`.
    #[serde(default)]
    pub class: String,
    /// Whether it has a spelling dictionary.
    #[serde(default)]
    pub spelling: bool,
    /// Whether it has hyphenation patterns.
    #[serde(default)]
    pub hyphenation: bool,
}

/// An installed pack, as recorded when it was installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledLanguagePack {
    /// Lowercase BCP 47 tag; the folder under [`LANGUAGE_PACKS`].
    pub locale: String,
    /// The language's own name.
    pub name: String,
    /// The release it came from.
    pub release: String,
    /// The craft-i18n commit it was built from.
    pub version: String,
    /// SPDX licence expression.
    pub licence: String,
    /// The archive's sha256.
    pub sha256: String,
    /// The archive's size in bytes.
    pub size: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Installed {
    version: u32,
    packs: Vec<InstalledLanguagePack>,
}

impl PackIndex {
    /// Parses and checks an index: format 1, every locale a lowercase tag and listed once, every
    /// archive at most [`MAX_PACK_ARCHIVE_BYTES`] with a sha256. Archive URLs must be `https://`
    /// (with `local`, the developer override, also loopback `http://` or a path).
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse(bytes, false)
    }

    fn parse(bytes: &[u8], local: bool) -> Result<Self, Error> {
        let bad = |why: String| Error::Io { what: "reading the Language pack index".into(), source: std::io::Error::other(why) };
        let index: PackIndex = serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))?;
        if index.format != 1 {
            return Err(bad(format!("format {} is not supported", index.format)));
        }
        if index.packs.len() > MAX_PACKS {
            return Err(bad("too many packs".into()));
        }
        for (i, p) in index.packs.iter().enumerate() {
            if !is_locale(&p.locale) || index.packs.iter().take(i).any(|q| q.locale == p.locale) {
                return Err(bad(format!("pack {:?}: not a lowercase language tag, or listed twice", p.locale.chars().take(40).collect::<String>())));
            }
            if p.size > MAX_PACK_ARCHIVE_BYTES || !is_sha256(&p.sha256) {
                return Err(bad(format!("pack {}: bad size or sha256", p.locale)));
            }
            if !(p.url.starts_with("https://") || local && local_source(&p.url).is_some()) {
                return Err(bad(format!("pack {}: {} is not an https URL", p.locale, p.url.chars().take(200).collect::<String>())));
            }
        }
        Ok(index)
    }

    /// The pack for `locale`.
    pub fn pack(&self, locale: &str) -> Option<&PackEntry> {
        self.packs.iter().find(|p| p.locale == locale)
    }
}

/// Where the pack index comes from: the pin, or the developer override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IndexSource {
    /// The pinned URL, checked against the pinned sha256.
    Pinned(PackIndexPin),
    /// `CRAFT_LANGUAGE_PACK_INDEX`: no index pin; loopback `http://` and paths allowed.
    Override(String),
}

impl IndexSource {
    /// The pin, unless [`LANGUAGE_PACK_INDEX_ENV`] is set.
    fn from_env(pin: &PackIndexPin) -> Self {
        match std::env::var(LANGUAGE_PACK_INDEX_ENV).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            Some(v) => {
                log::warn!(
                    "{LANGUAGE_PACK_INDEX_ENV}={v}: the Language pack index is NOT the pinned one and its sha256 is not checked (testing only)"
                );
                Self::Override(v)
            }
            None => Self::Pinned(*pin),
        }
    }

    fn is_override(&self) -> bool {
        matches!(self, Self::Override(_))
    }
}

/// A source the developer override may read: `Ok(url)` for an `https://` URL or an `http://`
/// one on a loopback host, `Err(path)` for a local file path; `None` for anything else.
fn local_source(s: &str) -> Option<Result<&str, PathBuf>> {
    if s.starts_with("https://") {
        return Some(Ok(s));
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
        let name = if host.starts_with('[') {
            host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
        } else {
            host.split(':').next().unwrap_or_default().to_string()
        };
        return ["127.0.0.1", "localhost", "[::1]"].contains(&name.as_str()).then_some(Ok(s));
    }
    if s.contains("://") {
        return None;
    }
    let path = s.strip_prefix("file://").unwrap_or(s);
    Some(Err(PathBuf::from(path)))
}

/// A lowercase BCP 47 tag as packs are named (craft-proof's rule): ASCII lowercase letters and
/// digits in parts of 1 to 8 joined by `-`, at most 35 characters.
fn is_locale(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 35
        && s.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl FontStore {
    /// The folder of installed Language packs (`<store>/language-packs`): give it to
    /// `craft_proof::packs::set_packs_dir`.
    pub fn language_packs_dir(&self) -> PathBuf {
        self.dir().join(LANGUAGE_PACKS)
    }

    /// The pack index saved by the last [`FontStore::fetch_language_pack_index`], when it is
    /// `pin`'s (or the developer override is set).
    pub fn cached_language_pack_index(&self, pin: &PackIndexPin) -> Option<PackIndex> {
        self.cached_pack_index_from(&IndexSource::from_env(pin))
    }

    pub(crate) fn cached_pack_index_from(&self, source: &IndexSource) -> Option<PackIndex> {
        let bytes = read_limited(&self.dir().join(INDEX_CACHE).join(INDEX_CACHE_NAME), MAX_PACK_INDEX_BYTES)?;
        match source {
            IndexSource::Pinned(pin) if sha256_hex(&bytes) != pin.sha256 => None,
            _ => PackIndex::parse(&bytes, source.is_override()).ok(),
        }
    }

    /// Downloads the pack index `pin` names, checks it against the pin and saves it for
    /// [`FontStore::cached_language_pack_index`]. Needs the network on.
    pub fn fetch_language_pack_index(&self, pin: &PackIndexPin, cancel: &Cancel) -> Result<PackIndex, Error> {
        self.fetch_pack_index_from(&IndexSource::from_env(pin), cancel)
    }

    pub(crate) fn fetch_pack_index_from(&self, source: &IndexSource, cancel: &Cancel) -> Result<PackIndex, Error> {
        self.require_network()?;
        let bytes = match source {
            IndexSource::Pinned(pin) => {
                let check = |b: &[u8]| if sha256_hex(b) == pin.sha256 { Ok(()) } else { Err(VerifyError::Sha256) };
                self.fetch(&[pin.url.to_string()], MAX_PACK_INDEX_BYTES, "the Language pack index", cancel, &mut |_| {}, &check)?
            }
            IndexSource::Override(at) => self.get_local(at, MAX_PACK_INDEX_BYTES, "the Language pack index", cancel, &mut |_| {}, &|_| Ok(()))?,
        };
        let index = PackIndex::parse(&bytes, source.is_override())?;
        std::fs::create_dir_all(self.dir().join(INDEX_CACHE)).map_err(io("creating the index folder"))?;
        self.write_file(&format!("{INDEX_CACHE}/{INDEX_CACHE_NAME}"), &bytes)?;
        Ok(index)
    }

    /// A download under the developer override: an https or loopback http URL, or a file.
    fn get_local(
        &self,
        at: &str,
        max: u64,
        what: &str,
        cancel: &Cancel,
        on_bytes: &mut dyn FnMut(u64),
        check: &dyn Fn(&[u8]) -> Result<(), VerifyError>,
    ) -> Result<Vec<u8>, Error> {
        let bytes = match local_source(at) {
            Some(Ok(url)) if url.starts_with("https://") => return self.fetch(&[url.to_string()], max, what, cancel, on_bytes, check),
            Some(Ok(url)) => Client::allowing_http(Timeouts::default())
                .get(url, max, cancel, on_bytes)
                .map_err(|e| Error::Download { what: what.to_string(), reasons: vec![e.to_string()] })?,
            Some(Err(path)) => {
                let mut buf = Vec::new();
                std::fs::File::open(&path)
                    .and_then(|f| f.take(max.saturating_add(1)).read_to_end(&mut buf))
                    .map_err(io(format!("reading {}", path.display())))?;
                if buf.len() as u64 > max {
                    return Err(Error::Download { what: what.to_string(), reasons: vec![format!("larger than {max} bytes")] });
                }
                on_bytes(buf.len() as u64);
                buf
            }
            None => return Err(Error::BadUrl(at.chars().take(200).collect())),
        };
        check(&bytes).map_err(|error| Error::Verify { file: what.to_string(), error })?;
        Ok(bytes)
    }

    /// Downloads `pack` from `index`, checks its size and sha256, unpacks it and moves it into
    /// `language-packs/<locale>/` in one rename, replacing an older one. Needs the network on.
    /// Then call `craft_proof::packs::reload()`.
    pub fn install_language_pack(
        &self,
        index: &PackIndex,
        pack: &PackEntry,
        cancel: &Cancel,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<InstalledLanguagePack, Error> {
        let local = std::env::var(LANGUAGE_PACK_INDEX_ENV).is_ok_and(|v| !v.trim().is_empty());
        self.install_pack_from(local, index, pack, cancel, progress)
    }

    pub(crate) fn install_pack_from(
        &self,
        local: bool,
        index: &PackIndex,
        pack: &PackEntry,
        cancel: &Cancel,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<InstalledLanguagePack, Error> {
        self.require_network()?;
        // The entry is checked again here: callers may build one by hand.
        if !is_locale(&pack.locale) || !is_sha256(&pack.sha256) || pack.size > MAX_PACK_ARCHIVE_BYTES {
            return Err(Error::BadUrl(pack.locale.chars().take(40).collect()));
        }
        let what = format!("{}.tar.zst", pack.locale);
        let (size, sha) = (pack.size, pack.sha256.clone());
        let check = move |b: &[u8]| {
            if b.len() as u64 != size {
                Err(VerifyError::Size { expected: size, actual: b.len() as u64 })
            } else if sha256_hex(b) != sha {
                Err(VerifyError::Sha256)
            } else {
                Ok(())
            }
        };
        let mut on_bytes = |done: u64| progress(Progress { file: 0, files: 1, done, total: size });
        let bytes = if local {
            self.get_local(&pack.url, size, &what, cancel, &mut on_bytes, &check)?
        } else if pack.url.starts_with("https://") {
            self.fetch(std::slice::from_ref(&pack.url), size, &what, cancel, &mut on_bytes, &check)?
        } else {
            return Err(Error::BadUrl(pack.url.chars().take(200).collect()));
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.sweep_staging();
        let stage = Stage::new(self.dir())?;
        let unpacked = stage.dir.join(&pack.locale);
        unpack(&bytes, &unpacked).map_err(|e| Error::Io { what: format!("unpacking {what}"), source: std::io::Error::other(e) })?;
        let record = InstalledLanguagePack {
            locale: pack.locale.clone(),
            name: pack.name.chars().take(200).collect(),
            release: index.release.chars().take(200).collect(),
            version: pack.version.chars().take(200).collect(),
            licence: pack.licence.chars().take(200).collect(),
            sha256: pack.sha256.clone(),
            size: pack.size,
        };
        let _lock = self.lock()?;
        let packs = self.language_packs_dir();
        std::fs::create_dir_all(&packs).map_err(io("creating the Language packs folder"))?;
        let dest = packs.join(&pack.locale);
        let old = stage.dir.join(format!("{}.old", pack.locale));
        let had = std::fs::symlink_metadata(&dest).is_ok();
        if had {
            std::fs::rename(&dest, &old).map_err(io(format!("replacing the {} pack", pack.locale)))?;
        }
        if let Err(e) = std::fs::rename(&unpacked, &dest) {
            if had {
                let _ = std::fs::rename(&old, &dest);
            }
            return Err(io(format!("installing the {} pack", pack.locale))(e));
        }
        let mut installed = self.read_installed();
        installed.packs.retain(|p| p.locale != record.locale);
        installed.packs.push(record.clone());
        installed.packs.sort_by(|a, b| a.locale.cmp(&b.locale));
        if let Err(e) = self.write_installed(&installed) {
            // Put the old pack back, so folder and record agree.
            let _ = std::fs::remove_dir_all(&dest);
            if had {
                let _ = std::fs::rename(&old, &dest);
            }
            return Err(e);
        }
        Ok(record)
    }

    /// The installed Language packs whose folder is there, by locale.
    pub fn installed_language_packs(&self) -> Vec<InstalledLanguagePack> {
        let dir = self.language_packs_dir();
        self.read_installed().packs.into_iter().filter(|p| is_locale(&p.locale) && dir.join(&p.locale).is_dir()).collect()
    }

    /// Removes the pack for `locale`: its folder disappears at once (one rename) and is deleted
    /// after. No network needed. Then call `craft_proof::packs::reload()`.
    pub fn remove_language_pack(&self, locale: &str) -> Result<(), Error> {
        if !is_locale(locale) {
            return Err(Error::NotInstalled(locale.chars().take(40).collect()));
        }
        let stage = Stage::new(self.dir())?;
        let _lock = self.lock()?;
        let dest = self.language_packs_dir().join(locale);
        let mut installed = self.read_installed();
        let known = installed.packs.iter().any(|p| p.locale == locale);
        if !known && std::fs::symlink_metadata(&dest).is_err() {
            return Err(Error::NotInstalled(locale.to_string()));
        }
        if std::fs::symlink_metadata(&dest).is_ok() {
            std::fs::rename(&dest, stage.dir.join(locale)).map_err(io(format!("removing the {locale} pack")))?;
        }
        installed.packs.retain(|p| p.locale != locale);
        self.write_installed(&installed)
        // `stage` deletes the old folder when dropped.
    }

    fn read_installed(&self) -> Installed {
        read_limited(&self.dir().join(INSTALLED), MAX_PACK_INDEX_BYTES).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write_installed(&self, installed: &Installed) -> Result<(), Error> {
        let bytes = serde_json::to_vec_pretty(&Installed { version: 1, packs: installed.packs.clone() })
            .map_err(|e| io("saving the Language pack list")(std::io::Error::other(e)))?;
        self.write_file(INSTALLED, &bytes)
    }
}

/// Unpacks a `.tar.zst` pack into `dest` (created): regular files and folders only, at most two
/// levels deep (`LICENCES/<file>`), every path plain and relative, each file at most
/// [`MAX_PACK_FILE_BYTES`], all of them [`MAX_PACK_UNPACKED_BYTES`], at most
/// [`MAX_PACK_ENTRIES`]; `pack.toml` required.
pub(crate) fn unpack(archive: &[u8], dest: &Path) -> Result<(), String> {
    let zstd = ruzstd::decoding::StreamingDecoder::new(archive).map_err(|e| format!("not zstd: {e}"))?;
    // The decompressed stream is capped too: tar padding and headers beyond the files' bytes.
    let limited = zstd.take(MAX_PACK_UNPACKED_BYTES.saturating_add((MAX_PACK_ENTRIES as u64 + 2) * 1024));
    let mut tar = tar::Archive::new(limited);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let (mut total, mut count) = (0u64, 0usize);
    let mut written: Vec<PathBuf> = Vec::new();
    for entry in tar.entries().map_err(|e| format!("not a tar archive: {e}"))? {
        let mut entry = entry.map_err(|e| format!("damaged archive: {e}"))?;
        count += 1;
        if count > MAX_PACK_ENTRIES {
            return Err("too many files".into());
        }
        let raw = entry.path().map_err(|e| format!("bad file name: {e}"))?.into_owned();
        let rel = safe_entry_path(&raw).ok_or_else(|| format!("{} is outside the pack", raw.display()))?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            if rel.components().count() > 1 {
                return Err(format!("{}: folders nest at most one level", rel.display()));
            }
            if !rel.as_os_str().is_empty() {
                std::fs::create_dir_all(dest.join(&rel)).map_err(|e| e.to_string())?;
            }
            continue;
        }
        if !kind.is_file() {
            return Err(format!("{}: only files and folders are allowed", rel.display()));
        }
        if rel.components().count() > 2 || written.contains(&rel) {
            return Err(format!("{}: too deep, or listed twice", rel.display()));
        }
        let size = entry.header().size().map_err(|e| e.to_string())?;
        if size > MAX_PACK_FILE_BYTES {
            return Err(format!("{}: larger than {MAX_PACK_FILE_BYTES} bytes", rel.display()));
        }
        total = total.saturating_add(size);
        if total > MAX_PACK_UNPACKED_BYTES {
            return Err(format!("larger than {MAX_PACK_UNPACKED_BYTES} bytes unpacked"));
        }
        let path = dest.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::options().write(true).create_new(true).open(&path).map_err(|e| format!("{}: {e}", rel.display()))?;
        let copied = std::io::copy(&mut (&mut entry).take(MAX_PACK_FILE_BYTES), &mut out).map_err(|e| format!("{}: {e}", rel.display()))?;
        if copied != size {
            return Err(format!("{}: truncated", rel.display()));
        }
        written.push(rel);
    }
    if !written.iter().any(|p| p.as_os_str() == "pack.toml") {
        return Err("no pack.toml".into());
    }
    Ok(())
}

/// `p` as a plain relative path (`./` prefixes dropped), or `None` for a root, a drive, `..` or a
/// name that isn't UTF-8 or holds a backslash, colon or control character.
fn safe_entry_path(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(n) => {
                let n = n.to_str()?;
                if n.is_empty() || n.contains(['\\', ':']) || n.chars().any(char::is_control) {
                    return None;
                }
                out.push(n);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
