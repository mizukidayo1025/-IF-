//! The core's source names no clock, file, environment or thread of `std` (DoD 4.1-(5) of #152):
//! it builds for wasm32-unknown-unknown, where those are missing or panic, and its results never
//! depend on the machine. The helper threads come from rayon, behind the feature `parallel`.
//! `thread_local!`, `Mutex` and `OnceLock` are allowed: they work on one thread.

use std::fs;
use std::path::Path;

/// `std::` or `core::` followed by one of these, and `use std::{...}` naming one.
const MODULES: [&str; 4] = ["time", "fs", "env", "thread"];

fn sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The places in `line` that name one of `MODULES` of std.
fn named(line: &str) -> Vec<&'static str> {
    let code = line.split("//").next().unwrap_or("");
    let mut hits = Vec::new();
    for root in ["std::", "core::"] {
        for (at, _) in code.match_indices(root) {
            let rest = &code[at + root.len()..];
            if let Some(list) = rest.strip_prefix('{') {
                let list = list.split('}').next().unwrap_or(list);
                for item in list.split(',') {
                    let head = item.trim().split("::").next().unwrap_or("");
                    hits.extend(MODULES.iter().filter(|module| **module == head));
                }
                continue;
            }
            let head: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            hits.extend(MODULES.iter().filter(|module| **module == head));
        }
    }
    for name in ["Instant::", "SystemTime"] {
        if code.contains(name) {
            hits.push("time");
        }
    }
    hits
}

#[test]
fn the_core_names_no_clock_file_environment_or_thread() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 10);
    let mut found = Vec::new();
    for path in files {
        for (number, line) in fs::read_to_string(&path).unwrap().lines().enumerate() {
            for module in named(line) {
                found.push(format!("{}:{}: std::{module}", path.strip_prefix(&root).unwrap().display(), number + 1));
            }
        }
    }
    assert!(found.is_empty(), "the core must not use these (#152, DoD 4.1-(5)):\n{}", found.join("\n"));
}

#[test]
fn the_check_finds_each_form() {
    assert_eq!(named("use std::time::Instant;"), ["time"]);
    assert_eq!(named("let start = Instant::now();"), ["time"]);
    assert_eq!(named("let raw = std::fs::read(path);"), ["fs"]);
    assert_eq!(named("use std::{env, sync::Mutex};"), ["env"]);
    assert_eq!(named("core::thread::spawn"), ["thread"]);
    assert!(named("use std::sync::{Mutex, OnceLock};").is_empty());
    assert!(named("// std::thread in a comment").is_empty());
    assert!(named("thread_local! {").is_empty());
}
