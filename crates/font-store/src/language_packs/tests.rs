use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::store::Caller;

const RELEASE: &str = "https://github.com/storytold/craft-i18n/releases/download/language-packs-1/";

/// A local HTTP server answering 200 with the body set for a path, else 404.
struct Server {
    base: String,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let r = routes.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let r = r.clone();
                std::thread::spawn(move || answer(stream, &r));
            }
        });
        Self { base, routes }
    }

    fn serve(&self, path: &str, body: &[u8]) {
        self.routes.lock().unwrap().insert(format!("/{path}"), body.to_vec());
    }

    /// A client that maps the release URL onto this server.
    fn client(&self) -> Client {
        let t = Timeouts { connect: Duration::from_secs(2), response: Duration::from_secs(5), overall: Duration::from_secs(10) };
        Client::for_tests(t, vec![(RELEASE.into(), format!("{}release/", self.base))])
    }
}

fn answer(mut s: TcpStream, routes: &Mutex<HashMap<String, Vec<u8>>>) {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut b) {
            Ok(1) => head.push(b[0]),
            _ => return,
        }
    }
    let path = String::from_utf8_lossy(&head).split_whitespace().nth(1).unwrap_or("/").to_string();
    let body = routes.lock().unwrap().get(&path).cloned();
    match body {
        Some(body) => {
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
        None => {
            let _ = write!(s, "HTTP/1.1 404 Nope\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope");
        }
    }
}

fn temp(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("craft-language-packs-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A tar of `files` (name, kind, bytes), with raw header names so hostile ones can be written.
fn tar_of(files: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, kind, bytes) in files {
        let mut h = tar::Header::new_old();
        let field = &mut h.as_old_mut().name;
        field[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(*kind);
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        if *kind == tar::EntryType::Symlink || *kind == tar::EntryType::Link {
            h.set_link_name("/etc/passwd").unwrap();
        }
        h.set_cksum();
        b.append(&h, *bytes).unwrap();
    }
    b.into_inner().unwrap()
}

fn zst(bytes: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
}

const FILE: tar::EntryType = tar::EntryType::Regular;

/// A good pack archive for `locale`, with `marker` in its word list.
fn pack_archive(locale: &str, marker: &str) -> Vec<u8> {
    let toml = format!("locale = \"{locale}\"\nname = \"Test\"\nlicence = \"MIT\"\nclass = \"Embeddable\"\n");
    let dic = format!("2\nhuis\n{marker}\n");
    zst(&tar_of(&[
        ("pack.toml", FILE, toml.as_bytes()),
        ("spelling.aff", FILE, b"SET UTF-8\n"),
        ("spelling.dic", FILE, dic.as_bytes()),
        ("hyphenation.pat", FILE, b"1na n2at\n"),
        ("LICENCES/", tar::EntryType::Directory, b""),
        ("LICENCES/COPYING", FILE, b"Test licence."),
    ]))
}

fn entry(locale: &str, url: &str, archive: &[u8]) -> serde_json::Value {
    serde_json::json!({"locale": locale, "name": "Nederlands", "version": "abc", "url": url, "sha256": sha256_hex(archive),
        "size": archive.len(), "licence": "MIT", "class": "Embeddable", "spelling": true, "hyphenation": true})
}

fn index_json(packs: &[serde_json::Value]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"format": 1, "release": "language-packs-1", "packs": packs})).unwrap()
}

/// A store whose network is on, a server with the index and the `nl` pack, and the pin.
fn setup(name: &str) -> (FontStore, Server, PackIndexPin, Vec<u8>) {
    let server = Server::start();
    let archive = pack_archive("nl", "zzmarker");
    let index = index_json(&[entry("nl", &format!("{RELEASE}nl.tar.zst"), &archive)]);
    server.serve("release/nl.tar.zst", &archive);
    server.serve("release/index.json", &index);
    let sha: &'static str = Box::leak(sha256_hex(&index).into_boxed_str());
    let pin = PackIndexPin { url: "https://github.com/storytold/craft-i18n/releases/download/language-packs-1/index.json", sha256: sha };
    let store = FontStore::new(temp(name), Caller::User).with_client(server.client());
    store.enable_network().unwrap();
    (store, server, pin, archive)
}

#[test]
fn install_list_replace_and_remove_a_pack() {
    let (store, server, pin, _) = setup("install");
    let source = IndexSource::Pinned(pin);
    let index = store.fetch_pack_index_from(&source, &Cancel::new()).unwrap();
    assert_eq!(store.cached_pack_index_from(&source), Some(index.clone()));
    let nl = index.pack("nl").unwrap().clone();
    let mut seen = Vec::new();
    let got = store.install_pack_from(false, &index, &nl, &Cancel::new(), &mut |p| seen.push(p)).unwrap();
    assert_eq!((got.locale.as_str(), got.release.as_str(), got.licence.as_str()), ("nl", "language-packs-1", "MIT"));
    assert!(seen.last().is_some_and(|p| p.done == nl.size && p.total == nl.size));
    let dir = store.language_packs_dir().join("nl");
    assert_eq!(store.language_packs_dir(), store.dir().join("language-packs"));
    for f in ["pack.toml", "spelling.aff", "spelling.dic", "hyphenation.pat", "LICENCES/COPYING"] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    assert!(std::fs::read_to_string(dir.join("spelling.dic")).unwrap().contains("zzmarker"));
    assert_eq!(store.installed_language_packs(), vec![got]);
    // A newer archive replaces the old one in one move.
    let newer = pack_archive("nl", "zznewer");
    server.serve("release/nl.tar.zst", &newer);
    let newer_entry = PackEntry { sha256: sha256_hex(&newer), size: newer.len() as u64, ..nl.clone() };
    store.install_pack_from(false, &index, &newer_entry, &Cancel::new(), &mut |_| {}).unwrap();
    assert!(std::fs::read_to_string(dir.join("spelling.dic")).unwrap().contains("zznewer"));
    assert_eq!(store.installed_language_packs().len(), 1);
    assert!(std::fs::read_dir(store.dir().join(".staging")).into_iter().flatten().flatten().next().is_none(), "nothing left staged");
    store.remove_language_pack("nl").unwrap();
    assert!(!dir.exists() && store.installed_language_packs().is_empty());
    assert!(matches!(store.remove_language_pack("nl"), Err(Error::NotInstalled(_))));
    assert!(matches!(store.remove_language_pack("../x"), Err(Error::NotInstalled(_))));
}

#[test]
fn network_off_pins_and_checksums_are_enforced() {
    let (store, server, pin, archive) = setup("pins");
    let source = IndexSource::Pinned(pin);
    store.disable_network().unwrap();
    assert!(matches!(store.fetch_pack_index_from(&source, &Cancel::new()), Err(Error::NetworkOff)));
    store.enable_network().unwrap();
    // Another index than the pinned one.
    let wrong = IndexSource::Pinned(PackIndexPin { sha256: "0000000000000000000000000000000000000000000000000000000000000000", ..pin });
    assert!(matches!(store.fetch_pack_index_from(&wrong, &Cancel::new()), Err(Error::Verify { error: VerifyError::Sha256, .. })));
    let index = store.fetch_pack_index_from(&source, &Cancel::new()).unwrap();
    assert_eq!(store.cached_pack_index_from(&wrong), None, "the cache is the pinned index only");
    let nl = index.pack("nl").unwrap().clone();
    // An archive that isn't the one the index names.
    server.serve("release/nl.tar.zst", &pack_archive("nl", "tampered"));
    assert!(store.install_pack_from(false, &index, &nl, &Cancel::new(), &mut |_| {}).is_err());
    server.serve("release/nl.tar.zst", &archive);
    let bad_sha = PackEntry { sha256: sha256_hex(b"other"), ..nl.clone() };
    assert!(matches!(
        store.install_pack_from(false, &index, &bad_sha, &Cancel::new(), &mut |_| {}),
        Err(Error::Verify { error: VerifyError::Sha256, .. })
    ));
    assert!(store.installed_language_packs().is_empty() && !store.language_packs_dir().join("nl").exists());
    // Hand-made entries are checked again.
    for e in [PackEntry { locale: "../nl".into(), ..nl.clone() }, PackEntry { url: "http://example.org/nl.tar.zst".into(), ..nl.clone() }] {
        assert!(store.install_pack_from(false, &index, &e, &Cancel::new(), &mut |_| {}).is_err());
    }
    store.disable_network().unwrap();
    assert!(matches!(store.install_pack_from(false, &index, &nl, &Cancel::new(), &mut |_| {}), Err(Error::NetworkOff)));
}

#[test]
fn indexes_are_checked() {
    let a = pack_archive("nl", "x");
    let ok = index_json(&[entry("nl", &format!("{RELEASE}nl.tar.zst"), &a)]);
    assert_eq!(PackIndex::from_json(&ok).unwrap().packs.len(), 1);
    let bad = [
        serde_json::to_vec(&serde_json::json!({"format": 2, "release": "r", "packs": []})).unwrap(),
        index_json(&[entry("NL", &format!("{RELEASE}nl.tar.zst"), &a)]),
        index_json(&[entry("../x", &format!("{RELEASE}nl.tar.zst"), &a)]),
        index_json(&[entry("nl", &format!("{RELEASE}nl.tar.zst"), &a), entry("nl", &format!("{RELEASE}nl.tar.zst"), &a)]),
        index_json(&[entry("nl", "http://127.0.0.1/nl.tar.zst", &a)]),
        index_json(&[entry("nl", "/tmp/nl.tar.zst", &a)]),
        b"not json".to_vec(),
    ];
    for b in bad {
        assert!(PackIndex::from_json(&b).is_err(), "{}", String::from_utf8_lossy(&b));
    }
    // The developer override also takes loopback http and paths, never other http hosts.
    assert!(PackIndex::parse(&index_json(&[entry("nl", "http://127.0.0.1:9/nl.tar.zst", &a)]), true).is_ok());
    assert!(PackIndex::parse(&index_json(&[entry("nl", "/tmp/nl.tar.zst", &a)]), true).is_ok());
    assert!(PackIndex::parse(&index_json(&[entry("nl", "http://example.org/nl.tar.zst", &a)]), true).is_err());
    for (s, ok) in [
        ("http://127.0.0.1:8000/i.json", true),
        ("http://localhost/i.json", true),
        ("http://[::1]:8/i.json", true),
        ("http://127.0.0.1.evil.org/i.json", false),
        ("http://evil.org@127.0.0.1/i.json", true),
        ("http://127.0.0.1@evil.org/i.json", false),
        ("ftp://127.0.0.1/i.json", false),
    ] {
        assert_eq!(local_source(s).is_some(), ok, "{s}");
    }
}

/// The developer override (`CRAFT_LANGUAGE_PACK_INDEX`): an index from a local path or a
/// loopback http server, with no index pin, while archives are still checked against its sha256.
#[test]
fn developer_override_reads_a_local_index_without_the_pin() {
    let (store, server, _, archive) = setup("override");
    // A local file index pointing at a local archive.
    let dir = temp("override-files");
    std::fs::write(dir.join("nl.tar.zst"), &archive).unwrap();
    let index = index_json(&[entry("nl", &dir.join("nl.tar.zst").display().to_string(), &archive)]);
    std::fs::write(dir.join("index.json"), &index).unwrap();
    let source = IndexSource::Override(dir.join("index.json").display().to_string());
    let got = store.fetch_pack_index_from(&source, &Cancel::new()).unwrap();
    assert_eq!(store.cached_pack_index_from(&source), Some(got.clone()));
    store.install_pack_from(true, &got, got.pack("nl").unwrap(), &Cancel::new(), &mut |_| {}).unwrap();
    assert!(store.language_packs_dir().join("nl/spelling.dic").is_file());
    // A loopback http index and archive.
    let http_index = index_json(&[entry("nl", &format!("{}local/nl.tar.zst", server.base), &archive)]);
    server.serve("local/index.json", &http_index);
    server.serve("local/nl.tar.zst", &archive);
    let source = IndexSource::Override(format!("{}local/index.json", server.base));
    let got = store.fetch_pack_index_from(&source, &Cancel::new()).unwrap();
    store.install_pack_from(true, &got, got.pack("nl").unwrap(), &Cancel::new(), &mut |_| {}).unwrap();
    // The archive is still verified.
    server.serve("local/nl.tar.zst", &pack_archive("nl", "tampered"));
    assert!(store.install_pack_from(true, &got, got.pack("nl").unwrap(), &Cancel::new(), &mut |_| {}).is_err());
    // Only loopback http.
    let source = IndexSource::Override("http://example.org/index.json".into());
    assert!(matches!(store.fetch_pack_index_from(&source, &Cancel::new()), Err(Error::BadUrl(_))));
    // Without the variable the pin is used.
    let pin = PackIndexPin { url: "https://example.org/index.json", sha256: "00" };
    if std::env::var_os(LANGUAGE_PACK_INDEX_ENV).is_none() {
        assert_eq!(IndexSource::from_env(&pin), IndexSource::Pinned(pin));
    }
}

#[test]
fn hostile_archives_are_refused() {
    let toml: &[u8] = b"locale = \"nl\"\nlicence = \"MIT\"\n";
    let big = vec![0u8; 10];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("traversal", zst(&tar_of(&[("pack.toml", FILE, toml), ("../evil", FILE, b"x")]))),
        ("deep traversal", zst(&tar_of(&[("pack.toml", FILE, toml), ("LICENCES/../../evil", FILE, b"x")]))),
        ("absolute", zst(&tar_of(&[("pack.toml", FILE, toml), ("/tmp/evil", FILE, b"x")]))),
        ("symlink", zst(&tar_of(&[("pack.toml", FILE, toml), ("spelling.dic", tar::EntryType::Symlink, b"")]))),
        ("hard link", zst(&tar_of(&[("pack.toml", FILE, toml), ("spelling.dic", tar::EntryType::Link, b"")]))),
        ("too deep", zst(&tar_of(&[("pack.toml", FILE, toml), ("a/b/c", FILE, b"x")]))),
        ("duplicate", zst(&tar_of(&[("pack.toml", FILE, toml), ("pack.toml", FILE, toml)]))),
        ("backslash", zst(&tar_of(&[("pack.toml", FILE, toml), ("..\\evil", FILE, b"x")]))),
        ("no pack.toml", zst(&tar_of(&[("spelling.dic", FILE, b"1\nx\n")]))),
        ("not zstd", tar_of(&[("pack.toml", FILE, toml)])),
        ("not tar", zst(b"hello, this is not a tar archive at all")),
        ("empty", Vec::new()),
        ("truncated", zst(&tar_of(&[("pack.toml", FILE, toml), ("spelling.dic", FILE, &big)]))[..20].to_vec()),
    ];
    for (what, archive) in cases {
        let root = temp("hostile");
        let dest = root.join("nl");
        assert!(unpack(&archive, &dest).is_err(), "{what}");
        assert!(!root.join("evil").exists() && !root.parent().unwrap().join("evil").exists(), "{what}");
    }
    // Too many entries.
    let names: Vec<String> = (0..=MAX_PACK_ENTRIES).map(|i| format!("f{i}")).collect();
    let mut files: Vec<(&str, tar::EntryType, &[u8])> = vec![("pack.toml", FILE, toml)];
    files.extend(names.iter().map(|n| (n.as_str(), FILE, &b""[..])));
    assert!(unpack(&zst(&tar_of(&files)), &temp("many").join("nl")).is_err());
    // A header claiming a file larger than the cap is refused before anything is read.
    let mut t = tar::Header::new_gnu();
    t.set_path("pack.toml").unwrap();
    t.set_size(toml.len() as u64);
    t.set_cksum();
    let mut b = tar::Builder::new(Vec::new());
    b.append(&t, toml).unwrap();
    let mut tarball = b.into_inner().unwrap();
    tarball.truncate(tarball.len() - 1024); // drop the end marker
    let mut h = tar::Header::new_gnu();
    h.set_path("spelling.dic").unwrap();
    h.set_size(MAX_PACK_FILE_BYTES + 1);
    h.set_cksum();
    tarball.extend_from_slice(h.as_bytes());
    tarball.extend_from_slice(&[0u8; 1024]);
    let err = unpack(&zst(&tarball), &temp("huge").join("nl")).unwrap_err();
    assert!(err.contains("larger than"), "{err}");
    // A good one unpacks.
    let dest = temp("good").join("nl");
    unpack(&pack_archive("nl", "x"), &dest).unwrap();
    assert!(dest.join("LICENCES/COPYING").is_file());
}

/// A staging folder an earlier process with the same id left behind is cleared, so an unpack
/// (`create_new`) can't fail on, or mix in, its files.
#[test]
fn a_stale_stage_folder_is_cleared() {
    let store = temp("stale-stage");
    let staging = store.join(".staging");
    for n in 0..512 {
        let d = staging.join(format!("{}-{n}", std::process::id()));
        std::fs::create_dir_all(d.join("nl")).unwrap();
        std::fs::write(d.join("nl/pack.toml"), b"old").unwrap();
    }
    let stage = Stage::new(&store).unwrap();
    assert_eq!(std::fs::read_dir(&stage.dir).unwrap().count(), 0, "{}", stage.dir.display());
}
