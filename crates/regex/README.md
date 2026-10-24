# craft-regex

A PCRE2-style backtracking regex engine in pure Rust, for searches that need what a linear-time
engine can't do: backreferences, lookaround, recursion and subroutine calls, conditionals, atomic
groups and possessive quantifiers, and `\K`. WordCraft uses it for Regex mode; plain and
wildcard searches stay on `regex-automata`.

A backtracker can take exponential time, so every search spends a caller-owned `Budget` of
steps, and the pattern size, program size, nesting and recursion depth and backtrack stack are
all capped. Going over any of them returns `Error::TooComplex`, never a hang. The parser,
compiler and matcher keep their own heap stacks, so a deep pattern can't overflow the Rust
stack. The crate has no `unsafe` and denies panicking constructs, indexing and unchecked
arithmetic.

Its only dependency is `regex-syntax` (Unicode tables and simple case folding).

```rust
use craft_regex::{Budget, Options, Regex};

let newlines = ['\n', '\u{1D}'];
let re = Regex::new(r"\b(\w+) \1\b", &Options { newlines: &newlines, ..Options::default() })?;
let mut budget = Budget::new(10_000_000); // WordCraft: max(10M, 64 × bytes), shared per command
if let Some(caps) = re.captures_at(hay, 0..hay.len(), &mut budget)? {
    let whole = caps.get(0);    // Option<Range<usize>>, byte offsets
    let word = caps.get(1);
}
```

There's no iterator and no replace helper: callers loop over `captures_at` themselves (moving
past empty matches) and expand their own replacement syntax.

## Options

| Field | Default | Meaning |
|---|---|---|
| `case_insensitive`, `multiline`, `dot_all` | `false` | `(?i)`, `(?m)`, `(?s)` for the whole pattern |
| `newlines` | `['\n']` | The chars that end a line, for `^ $ . \N` |
| `max_depth` | 1,000 | Group nesting while parsing; recursion depth while matching |
| `max_stack` | 1,000,000 | Backtrack stack entries (call frames count by size) |
| `max_pattern` | 1,000 | Pattern length in chars |
| `max_insts` | 10,000 | Compiled program size; counted repeats are unrolled |

`Budget::new(steps)` belongs to the caller. Share one across several searches to cap them
together; `remaining()` tells how much is left.

## Supported syntax

- **Literals and escapes:** any char; `\t \n \r \e \f \a`; `\xhh`, `\x{h…}`, `\o{…}`, `\0oo`,
  `\cX`; `\` before any non-alphanumeric char; `\Q…\E`.
- **Classes:** `[…]` and `[^…]` with ranges, escapes, `\p{…}`/`\P{…}`/`\pL`/`\p{^…}`, POSIX
  `[:alpha:]` and `[:^alpha:]`; `\d \w \s \h \v` and `\D \W \S \H \V`; `.`, `\N`, `\R`.
- **Anchors:** `^ $ \A \z \Z \b \B`.
- **Groups:** `( )`, `(?: )`, named `(?<n> )`, `(?'n' )`, `(?P<n> )`, branch reset `(?| )`,
  atomic `(?> )`, comments `(?# )`.
- **Alternation** `|`.
- **Quantifiers:** `* + ? {n} {n,} {n,m}`, each greedy, lazy (`?`) or possessive (`+`).
- **Backreferences:** `\1`… (`\10` and up are octal unless that many groups exist), `\g1`,
  `\g{1}`, `\g{-1}`, `\g{name}`, `\k<n>`, `\k'n'`, `\k{n}`, `(?P=n)`.
- **Lookaround:** `(?= )`, `(?! )`, `(?<= )`, `(?<! )`. A lookbehind may vary in length but must
  be bounded, at most 255 chars.
- **Recursion and subroutines:** `(?R)`, `(?0)`, `(?1)`, `(?-1)`, `(?+1)`, `(?&n)`, `(?P>n)`,
  `\g<n>`, `\g'n'`.
- **Conditionals:** `(?(1)…|…)`, `(?(-1)…)`, `(?(<n>)…)`, `(?('n')…)`, `(?(n)…)`, `(?(R)…)`,
  `(?(R1)…)`, `(?(R&n)…)`, `(?(DEFINE)…)`, and assertions `(?(?=…)…)`, `(?(?!…)…)`,
  `(?(?<=…)…)`, `(?(?<!…)…)`.
- **`\K`** (not inside lookaround).
- **Flags:** `(?i) (?m) (?s) (?x) (?n)`, turned on or off (`(?i-m)`), to the end of the group or
  scoped `(?i: )`.

**Unsupported** (`Error::Unsupported`): `(*VERB)`s, callouts `(?C)`, `\C`, `\G`, `\X`, `(?U)`,
`(?J)`, `(?X)`, `(?^)`, and `\Q` inside a class.

## Semantics

- Leftmost-first (Perl), not leftmost-longest. The haystack is read one `char` at a time;
  positions are byte offsets on char boundaries.
- `captures_at(hay, range, …)`: the match lies inside `range`. Text after `range.end` is not
  seen at all, as if `hay` ended there; text before `range.start` is seen by lookbehind, `\b`
  and `^`. An invalid range (reversed, out of bounds, off a char boundary) has no match.
- `\d \w \s \b` are Unicode-aware (like `regex-automata`, so Regex mode keeps its behaviour);
  POSIX classes are ASCII. `(?i)` uses simple case folding: `k` matches `K` and `K` (Kelvin),
  but `ß` doesn't match `ss`.
- `$` and `\Z` also match before a final newline; `(?m)^` doesn't match after a newline that ends
  the text (both PCRE2).
- Recursion follows PCRE2: groups set inside a recursion or subroutine call revert when it
  returns, and the engine can backtrack into a call. An unset group never matches as a
  backreference.
- A loop ends after an iteration that matched nothing, keeping that iteration's captures (PCRE2;
  `regex-automata` drops them, e.g. `(?:(a|))*` on `a`).
- Atomic groups and lookarounds keep the captures set inside them when they succeed; negative
  lookarounds never set captures.
- At compile time the engine works out which chars a match can start with, and the search
  skips positions that can't start one.

## Tests

- `tests/features.rs`: hand-written cases for every construct and error.
- `tests/differential.rs`: random patterns from the subset shared with `regex-automata` (a
  dev-dependency) must give the same match and groups.
- `tests/hostile.rs`: catastrophic patterns, recursion without a base case, 50,000-deep nesting,
  10,000 alternatives, huge repeats, bad ranges and a junk-pattern proptest. Each must return
  `Ok` or `Err` without panicking, in under 100 ms with a 10M-step budget. The workspace
  builds this crate optimized in the dev profile so that holds under `cargo test`.

The tests are written from the PCRE2 documentation; no PCRE2 code or test data is used.

Check the wasm build with:

```sh
cargo build -p craft-regex --target wasm32-unknown-unknown
```
