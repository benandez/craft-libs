//! Regenerate `data/en-words.dic`, the English word list built into the crate (see the crate
//! README): every word of the public-domain Moby Hyphenator II list, without its break points.
//!
//! ```sh
//! cargo run -p craft-proof --example en_words -- <craft-i18n>/language-data/en-us/en-us.dic data/en-words.dic
//! ```
//!
//! The input is craft-i18n's `en-us.dic`: the Moby Hyphenator II list by Grady Ward (Project
//! Gutenberg eBook #3204, `mhyph.txt`), already stripped of the Project Gutenberg header and
//! licence, with its break points. The output keeps the words only (in [`Dictionary`]'s format,
//! every break mask zero), so it is smaller and carries no hyphenation. No other word list or
//! dictionary is read.

use craft_proof::dict::Dictionary;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dic, out] = args.as_slice() else {
        return Err("usage: en_words <en-us.dic> <out>".into());
    };
    let bytes = std::fs::read(dic).map_err(|e| format!("{dic}: {e}"))?;
    let moby = Dictionary::from_bytes(&bytes)?;
    let words = Dictionary::from_entries(moby.iter().map(|(w, _)| (w.to_string(), 0)).collect());
    let data = words.to_bytes();
    std::fs::write(out, &data).map_err(|e| format!("{out}: {e}"))?;
    eprintln!("{} words, {} bytes", words.len(), data.len());
    Ok(())
}
