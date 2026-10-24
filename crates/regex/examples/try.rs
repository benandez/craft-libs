//! Try a pattern from the command line:
//!
//! ```sh
//! cargo run -p craft-regex --example try -- '\b(\w+) \1\b' 'this is is a test'
//! ```
//!
//! Prints every match and its groups. Add `-i` before the pattern for case-insensitive.

use craft_regex::{Budget, Options, Regex};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let ci = args.first().is_some_and(|a| a == "-i");
    if ci {
        args.remove(0);
    }
    let [pattern, text] = args.as_slice() else {
        eprintln!("usage: try [-i] <pattern> <text>");
        std::process::exit(2);
    };
    let re = match Regex::new(pattern, &Options { case_insensitive: ci, ..Options::default() }) {
        Ok(re) => re,
        Err(e) => {
            eprintln!("bad pattern: {e}");
            std::process::exit(1);
        }
    };
    let mut budget = Budget::new(10_000_000);
    let mut at = 0;
    let mut found = 0;
    while at <= text.len() {
        let caps = match re.captures_at(text, at..text.len(), &mut budget) {
            Ok(Some(caps)) => caps,
            Ok(None) => break,
            Err(e) => {
                eprintln!("search stopped: {e}");
                std::process::exit(1);
            }
        };
        let Some(whole) = caps.get(0) else { break };
        found += 1;
        println!("match {found}: {:?} at bytes {}..{}", &text[whole.clone()], whole.start, whole.end);
        for i in 1..caps.len() {
            match caps.get(i) {
                Some(g) => println!("  group {i}: {:?}", &text[g]),
                None => println!("  group {i}: (not used)"),
            }
        }
        // Move past this match; after an empty match, step over one char.
        at = if whole.is_empty() { whole.end + text[whole.end..].chars().next().map_or(1, char::len_utf8) } else { whole.end };
    }
    if found == 0 {
        println!("no match");
    }
}
