# Examples & Benchmarks Refactoring

## Dependencies

This plan assumes the following known-issues are resolved first:

- **#1** (remove legacy re-exports) — `bench.rs` accesses internal fields (`dict.idx`, `dict.dict`, `dict.syn`) through the re-exports. The new benchmark uses only the `Dictionary` trait.
- **#2** (take `impl AsRef<Path>`) — all new code passes `&str` / `&Path` instead of `PathBuf`.
- **#3** (auto-detect format) — all new code calls `opendict::open(path)` with no `DictKind` argument. The library scans for `.ifo` / `.mdx` and picks the right format.

## Summary

| File | Action |
|---|---|
| `examples/bench.rs` | Delete. Replaced by `benches/dictionary.rs` with criterion. |
| `examples/verify_search.rs` | Delete. Logic moves to `tests/real_dicts.rs` as `#[ignore]` tests. |
| `examples/lookup_words.rs` | Delete. Replaced by `examples/lookup.rs`. |
| `examples/show_dicts.rs` | Delete. Replaced by `examples/show_dict.rs`. |
| `examples/gen_mdx_fixture.rs` | Keep as-is. Development tool, not user-facing, but fine in `examples/`. |
| `benches/dictionary.rs` | New. Criterion benchmarks for open/lookup/prefix_search. |
| `examples/lookup.rs` | New. Minimal single-dictionary lookup from CLI args. |
| `examples/show_dict.rs` | New. Show metadata and sample entries from CLI arg. |

## 1. Cargo.toml changes

Add criterion as a dev-dependency and register the benchmark target:

```toml
[dev-dependencies]
tempfile = "3"
criterion = "0.5"

[[bench]]
name = "dictionary"
harness = false
```

## 2. Delete `examples/verify_search.rs`, add tests to `tests/real_dicts.rs`

This is a correctness check (verify every indexed word is findable via binary search). It belongs as an integration test, not an example.

Delete the example and add two `#[ignore]` tests to `tests/real_dicts.rs`, placed after the existing `stardict_lookup_first_last_middle_word` and `mdict_lookup_first_last_middle_word` tests respectively.

### After `stardict_lookup_first_last_middle_word` (line 171):

```rust
#[test]
#[ignore]
fn stardict_verify_all_lookups() {
    let dicts = find_stardict_dirs();
    for (name, dir) in &dicts {
        let dict = Dictionary::open(dir.clone(), name).unwrap();
        let words = dict.word_list();
        let mut failures = 0;
        for word in &words {
            if dict.lookup(word).is_none() {
                failures += 1;
            }
        }
        assert_eq!(
            failures, 0,
            "StarDict '{}': {}/{} words not found via lookup",
            name, failures, words.len()
        );
    }
}
```

### After `mdict_lookup_first_last_middle_word` (line 253):

```rust
#[test]
#[ignore]
fn mdict_verify_all_lookups() {
    let dicts = find_mdict_dirs();
    for (name, dir) in &dicts {
        let dict = match opendict::open(dir.clone(), opendict::DictKind::MDict) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping MDict '{}': {}", name, e);
                continue;
            }
        };
        let words = dict.word_list();
        let mut failures = 0;
        for word in &words {
            if dict.lookup(word).is_none() {
                failures += 1;
            }
        }
        assert_eq!(
            failures, 0,
            "MDict '{}': {}/{} words not found via lookup",
            name, failures, words.len()
        );
    }
}
```

## 3. Delete `examples/bench.rs`

Replaced entirely by `benches/dictionary.rs`. Problems with the current file:

- Accesses internal struct fields (`dict.idx.entry_count()`, `dict.dict.data_len()`, etc.) that will be removed with known-issue #1.
- Uses two different legacy API paths (`opendict::dictionary::Dictionary` and `opendict::open(dir, DictKind)`).
- macOS-only RSS measurement via shelling out to `ps`.
- Single-pass timing with no warmup or statistical analysis.
- Hardcoded paths to 8 specific dictionaries.

## 4. Delete `examples/lookup_words.rs`, create `examples/lookup.rs`

The old file hardcodes 4 dictionaries and specific word lists per language. The replacement takes a path and word from CLI args.

```rust
//! Look up a word in any StarDict or MDict dictionary.
//!
//! Usage:
//!     cargo run --example lookup -- /path/to/dict "word"

use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <dict-path> <word>", args[0]);
        process::exit(1);
    }

    let dict = opendict::open(&args[1]).unwrap_or_else(|e| {
        eprintln!("Failed to open dictionary: {e}");
        process::exit(1);
    });

    let info = dict.info();
    println!("{} ({} words)\n", info.name, dict.word_count());

    match dict.lookup(&args[2]) {
        Some(entries) => {
            for e in &entries {
                let text = String::from_utf8_lossy(&e.data);
                let preview: String = text.chars().take(200).collect();
                let ellipsis = if text.chars().count() > 200 { "..." } else { "" };
                println!("  [{}] {}{}", e.type_id, preview.trim(), ellipsis);
            }
        }
        None => println!("  (not found)"),
    }
}
```

## 5. Delete `examples/show_dicts.rs`, create `examples/show_dict.rs`

The old file hardcodes `tests/dicts/stardict/` and only works with StarDict. The replacement takes a path from CLI args and works with any format.

```rust
//! Show metadata and sample entries from a dictionary.
//!
//! Usage:
//!     cargo run --example show_dict -- /path/to/dict

use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <dict-path>", args[0]);
        process::exit(1);
    }

    let dict = opendict::open(&args[1]).unwrap_or_else(|e| {
        eprintln!("Failed to open dictionary: {e}");
        process::exit(1);
    });

    let info = dict.info();
    println!("Name:    {}", info.name);
    println!("Words:   {}", dict.word_count());
    if !info.author.is_empty() {
        println!("Author:  {}", info.author);
    }
    if !info.description.is_empty() {
        let desc: String = info.description.chars().take(200).collect();
        println!("Desc:    {}", desc.trim());
    }
    println!();

    let words = dict.word_list();
    let indices = sample_indices(words.len(), 10);

    for i in indices {
        let word = &words[i];
        print!("  [{i}] {word:?}");
        match dict.lookup(word) {
            Some(entries) => {
                for e in &entries {
                    let text = String::from_utf8_lossy(&e.data);
                    let preview: String = text.chars().take(120).collect();
                    let ellipsis = if text.chars().count() > 120 { "..." } else { "" };
                    print!("  [{}] {}{}", e.type_id, preview.trim(), ellipsis);
                }
                println!();
            }
            None => println!("  NOT FOUND"),
        }
    }
}

/// Pick up to `n` evenly-spaced indices spanning the full range.
fn sample_indices(len: usize, n: usize) -> Vec<usize> {
    if len == 0 {
        return vec![];
    }
    if len <= n {
        return (0..len).collect();
    }
    (0..n).map(|i| i * (len - 1) / (n - 1)).collect()
}
```

## 6. Create `benches/dictionary.rs`

Criterion benchmark that discovers dictionaries from the `OPENDICT_BENCH_DIR` environment variable. Each subdirectory should contain exactly one dictionary (StarDict or MDict files).

Run with:
```sh
OPENDICT_BENCH_DIR=tests/dicts cargo bench
```

Or point at any directory of dictionaries:
```sh
OPENDICT_BENCH_DIR=/path/to/my/dicts cargo bench
```

### Benchmark groups

| Group | What it measures |
|---|---|
| `open` | Time to load a dictionary from disk (includes I/O, parsing, mmap). |
| `lookup_hit` | Lookup speed for words that exist. Samples up to 1000 evenly-spaced words from the index. |
| `lookup_miss` | Lookup speed for words that don't exist. Uses 1000 synthetic `__miss_N__` keys. |
| `prefix_search` | Prefix search speed. Takes 2-char prefixes from 100 evenly-spaced words, limit 20 results each. |

### Code

```rust
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

/// Discover dictionary directories from OPENDICT_BENCH_DIR.
///
/// Each subdirectory should contain one dictionary (StarDict or MDict).
/// Returns an empty vec if the env var is not set.
fn discover_dicts() -> Vec<(String, PathBuf)> {
    let dir = match std::env::var("OPENDICT_BENCH_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => {
            eprintln!(
                "note: set OPENDICT_BENCH_DIR to a directory of dictionaries to benchmark"
            );
            return vec![];
        }
    };

    let mut dicts = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("cannot read OPENDICT_BENCH_DIR") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            let name = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            dicts.push((name, path));
        }
    }
    dicts.sort_by(|a, b| a.0.cmp(&b.0));
    dicts
}

/// Sample up to `n` evenly-spaced items from a slice.
fn sample(words: &[String], n: usize) -> Vec<&str> {
    if words.is_empty() {
        return vec![];
    }
    if words.len() <= n {
        return words.iter().map(|s| s.as_str()).collect();
    }
    (0..n)
        .map(|i| words[i * (words.len() - 1) / (n - 1)].as_str())
        .collect()
}

fn bench_open(c: &mut Criterion) {
    let dicts = discover_dicts();
    if dicts.is_empty() {
        return;
    }

    let mut group = c.benchmark_group("open");
    for (name, path) in &dicts {
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| opendict::open(black_box(path)).unwrap())
        });
    }
    group.finish();
}

fn bench_lookup_hit(c: &mut Criterion) {
    let dicts = discover_dicts();
    if dicts.is_empty() {
        return;
    }

    let mut group = c.benchmark_group("lookup_hit");
    for (name, path) in &dicts {
        let dict = opendict::open(path).unwrap();
        let words = dict.word_list();
        if words.is_empty() {
            continue;
        }
        let sample = sample(&words, 1000);

        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                for w in &sample {
                    black_box(dict.lookup(w));
                }
            })
        });
    }
    group.finish();
}

fn bench_lookup_miss(c: &mut Criterion) {
    let dicts = discover_dicts();
    if dicts.is_empty() {
        return;
    }

    let miss_words: Vec<String> = (0..1000).map(|i| format!("__miss_{i}__")).collect();

    let mut group = c.benchmark_group("lookup_miss");
    for (name, path) in &dicts {
        let dict = opendict::open(path).unwrap();

        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                for w in &miss_words {
                    black_box(dict.lookup(w));
                }
            })
        });
    }
    group.finish();
}

fn bench_prefix_search(c: &mut Criterion) {
    let dicts = discover_dicts();
    if dicts.is_empty() {
        return;
    }

    let mut group = c.benchmark_group("prefix_search");
    for (name, path) in &dicts {
        let dict = opendict::open(path).unwrap();
        let words = dict.word_list();
        if words.is_empty() {
            continue;
        }

        let prefixes: Vec<String> = sample(&words, 100)
            .iter()
            .map(|w| w.chars().take(2).collect())
            .collect();

        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                for p in &prefixes {
                    black_box(dict.search_prefix(p, 20));
                }
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_open,
    bench_lookup_hit,
    bench_lookup_miss,
    bench_prefix_search
);
criterion_main!(benches);
```

## 7. Keep `examples/gen_mdx_fixture.rs`

No changes. This is a development tool for generating `tests/fixtures/test.mdx`. It's well-documented, self-contained, and useful. Run with `cargo run --example gen_mdx_fixture`.
