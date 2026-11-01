# craft-proof

Proofing for the Craft apps, per language: spelling, suggestions, grammar checks and hyphenation,
a Hunspell `.aff`/`.dic` reader, user words, ignored words and a dictionaries folder. It moved here
from WordCraft's `wordcraft-proof` with its history and the same public API. No `unsafe`, no
panics on hostile input, and it builds for `wasm32-unknown-unknown`.

```toml
craft-proof = { git = "https://github.com/storytold/craft-libs", rev = "<commit>" }
```

## Languages

| Language | Spelling | Grammar | Hyphenation |
|---|---|---|---|
| English (en-US, en-GB and the British-spelling Englishes, en-CA) | Moby word list + `en-extra.txt`, dialect variants (`en-variants.txt`); without them the installed Hunspell English, else the built-in core list ([below](#english-is-always-checked)) | yes | Moby list, our patterns, heuristic |
| Polish, Portuguese, Italian | the installed Hunspell dictionary | yes | TeX patterns |
| Croatian, Turkish, Russian, Czech, Indonesian, Spanish (es, es-MX, es-419, es-UY) | the installed Hunspell dictionary (Russian also `ru-extra.txt`) | yes | Turkish and Russian rules; English fallback otherwise |
| German | — | — | TeX patterns |
| Bulgarian | — | — | syllable rules |

Grammar rules, suggestion tuning and the rule-based hyphenation are code: they work in every
build.

## Use

```rust
use craft_proof::{ProofLang, check_text_with, suggest_in, Options};
use craft_proof::hyphen::{hyphen_points_in, Limits};

// A paragraph whose runs are in different languages (byte ranges of `text`).
let issues = check_text_with(text, &[(0..text.len(), ProofLang::EnGb)], &Options::default(), true, false);
let fixes = suggest_in("colour", 5, ProofLang::En);
let breaks = hyphen_points_in("hyphenation", &Limits::default(), ProofLang::En);
```

State is process-wide: `add_word`, `ignore_word`, `set_dictionaries_folder`,
`load_user_dictionary`/`save_user_dictionary`. The crate reads no document model; callers pass
plain text and language spans.

## Language data

The word lists and patterns live in [craft-i18n](https://github.com/storytold/craft-i18n)
(`language-data/`), not here. `build.rs` embeds them at build time:

- `CRAFT_I18N_DIR=<absolute path to a craft-i18n checkout>` embeds `language-data/`.
- Unset (a fork, docs.rs, offline): the crate builds with a warning and no embedded Language
  data. English spelling then falls back as below, hyphenation that needs the data is off, and
  grammar and the Hunspell languages work as before.
- `CRAFT_I18N_REQUIRED=1` (releases) makes a missing directory or file an error.

Tests that pin results of the data are ignored without it; `src/no_data.rs` runs every public
entry point with no data embedded.

### English is always checked

English spelling uses the first of these that is there; `english_source(lang)` says which.

1. **Embedded:** the Moby word list from `CRAFT_I18N_DIR` (above). Nothing below is used then.
2. **System Hunspell:** an English `.aff`/`.dic` pair in the dictionaries folder or the system
   locations, the run's English first (`en_US`, `en_GB`, `en_CA`…), then another English. None on
   the web.
3. **Core list:** `data/en-core.txt`, about 11,000 common words built into the crate (inflections
   come from the same rules as the Moby list's). It marks only near misses of common words: two
   letters swapped (`teh`, `recieved`), or one letter added, missing or changed in a lower-case
   word of four letters or more (`definately`). Other words it doesn't know (rare words, names,
   terms) are taken as right.

A dictionary that is there but can't be read (encoding, size, parse error, no words) is skipped,
and `dictionary_load_error(lang)` gives the reason; it never panics. Text in other languages is
never checked against English: a language without its dictionary has no spelling marks.

The core list is public domain. Every word is from the Moby Hyphenator II list by Grady Ward
(Project Gutenberg eBook #3204, as in craft-i18n's `language-data/en-us/en-us.dic`); 50
public-domain Project Gutenberg books only rank them by how often they are written in lower case.
No other word list or dictionary was used. Regenerate with
`cargo run -p craft-proof --example en_core -- <en-us.dic> 12000 crates/proof/data/en-core.txt <books…>`.

## Check

```sh
cargo fmt -p craft-proof --check
cargo clippy -p craft-proof --all-targets -- -D warnings
CRAFT_I18N_DIR=/abs/path/craft-i18n cargo test -p craft-proof
cargo test -p craft-proof   # without Language data
cargo check -p craft-proof --target wasm32-unknown-unknown
```

CI: `.github/workflows/proof.yml`.

## Licence

MIT OR Apache-2.0. The Language data carries its own licences, recorded in craft-i18n's
`ATTRIBUTION.md`.
