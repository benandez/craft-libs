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
| English (en-US, en-GB and the British-spelling Englishes, en-CA) | Moby word list + `en-extra.txt`, dialect variants (`en-variants.txt`) | yes | Moby list, our patterns, heuristic |
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
  data. English spelling then marks nothing and suggests nothing, and hyphenation that needs the
  data is off; grammar and the Hunspell languages work as before.
- `CRAFT_I18N_REQUIRED=1` (releases) makes a missing directory or file an error.

Tests that pin results of the data are ignored without it; `src/no_data.rs` runs every public
entry point with no data embedded.

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
