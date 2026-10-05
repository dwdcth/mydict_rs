# Plan: Drop `failure` and `log` dependencies

## Why

- `failure` is deprecated (unmaintained since 2020), causes `non_local_definitions` warnings on every build
- `log` is only used for 2 `warn!` calls — not worth the dep for a library this small
- Replace with `anyhow` (for `Result<T>` + `anyhow!` macro) — zero-cost, modern, maintained

## Scope

7 files touched. No test changes needed — tests only check `is_err()` / `is_ok()`, not error message strings.

---

## Cargo.toml

```toml
# BEFORE
[dependencies]
log = "0.4"
failure = "0.1"

# AFTER
[dependencies]
anyhow = "1"
```

## src/result.rs

```rust
// BEFORE
use std::result::Result as StdResult;

use failure::{Error as FailureError, Fail};

pub type Result<T> = StdResult<T, FailureError>;

#[derive(Fail, Debug)]
pub enum Error {
    #[fail(display = "{}", _0)]
    Io(#[fail(cause)] std::io::Error),
    #[fail(display = "{}", _0)]
    Utf8(#[fail(cause)] std::str::Utf8Error),
}

// AFTER
pub type Result<T> = anyhow::Result<T>;
```

## src/lib.rs

```rust
// BEFORE
#[macro_use]
extern crate log;
#[macro_use]
extern crate failure;

pub mod dict;
pub mod dictionary;
pub mod idx;
pub mod ifo;
pub mod result;
pub mod strcmp;
pub mod syn;

use std::{fs, path};

pub struct StarDict {
    directories: Vec<dictionary::Dictionary>,
}

impl StarDict {
    pub fn new(root: path::PathBuf) -> result::Result<StarDict> {
        let mut items = Vec::new();
        if root.is_dir() {
            for it in fs::read_dir(root)? {
                let it = it?.path();
                if it.is_dir() {
                    match dictionary::Dictionary::new(it) {
                        Ok(it) => {
                            items.push(it);
                        }
                        Err(e) => {
                            error!("ignore reason: {}", e);
                        }
                    }
                }
            }
        }

        Ok(StarDict { directories: items })
    }

    pub fn info(&mut self) -> Vec<ifo::Ifo> {
        let mut items = Vec::new();
        for it in &mut self.directories {
            items.push(it.ifo.clone());
        }
        items
    }

    pub fn search(&mut self, word: &str) -> Vec<dictionary::Translation> {
        let mut items = Vec::new();
        for it in &mut self.directories {
            match it.search(word) {
                Ok(v) => items.push(dictionary::Translation {
                    info: it.ifo.clone(),
                    results: v,
                }),
                Err(e) => warn!("search {} in {} failed: {}", word, it.ifo.name, e),
            }
        }
        items
    }
}

// AFTER
pub mod dict;
pub mod dictionary;
pub mod idx;
pub mod ifo;
pub mod result;
pub mod strcmp;
pub mod syn;

use std::{fs, path};

pub struct StarDict {
    directories: Vec<dictionary::Dictionary>,
}

impl StarDict {
    pub fn new(root: path::PathBuf) -> result::Result<StarDict> {
        let mut items = Vec::new();
        if root.is_dir() {
            for it in fs::read_dir(root)? {
                let it = it?.path();
                if it.is_dir() {
                    match dictionary::Dictionary::new(it) {
                        Ok(it) => {
                            items.push(it);
                        }
                        Err(_) => {}
                    }
                }
            }
        }

        Ok(StarDict { directories: items })
    }

    pub fn info(&mut self) -> Vec<ifo::Ifo> {
        let mut items = Vec::new();
        for it in &mut self.directories {
            items.push(it.ifo.clone());
        }
        items
    }

    pub fn search(&mut self, word: &str) -> Vec<dictionary::Translation> {
        let mut items = Vec::new();
        for it in &mut self.directories {
            match it.search(word) {
                Ok(v) => items.push(dictionary::Translation {
                    info: it.ifo.clone(),
                    results: v,
                }),
                Err(_) => {}
            }
        }
        items
    }
}
```

## src/ifo.rs

Only change: `format_err!()` → `anyhow::anyhow!()`, drop `warn!()`.

```rust
// BEFORE
use super::result::Result;
// ... and throughout:
//   format_err!("...")
//   warn!("Ignore line: {}", line)

// AFTER
use anyhow::anyhow;

use super::result::Result;
// ... and throughout:
//   anyhow!("...")
//   (drop the warn! line, just ignore unknown keys silently)
```

Full replacement of each `format_err!` call:

```
line 53:  format_err!("invalid magic line: {}", trimmed)       → anyhow!("invalid magic line: {}", trimmed)
line 71:  format_err!("unsupported version: {}", v)            → anyhow!("unsupported version: {}", v)
line 89:  _ => warn!("Ignore line: {}", line),                 → _ => {}
line 95:  format_err!("missing required field: bookname")      → anyhow!("missing required field: bookname")
line 98:  format_err!("missing required field: wordcount")     → anyhow!("missing required field: wordcount")
line 101: format_err!("missing required field: idxfilesize")   → anyhow!("missing required field: idxfilesize")
```

## src/idx.rs

Only change: `format_err!()` → `anyhow::anyhow!()`.

```
line 27:  format_err!("idx: missing null terminator at offset {}", pos)  → anyhow!("idx: missing null terminator at offset {}", pos)
line 35:  format_err!("idx: unexpected EOF reading 64-bit entry")        → anyhow!("idx: unexpected EOF reading 64-bit entry")
line 50:  format_err!("idx: unexpected EOF reading 32-bit entry")        → anyhow!("idx: unexpected EOF reading 32-bit entry")
```

Add `use anyhow::anyhow;` at top, remove nothing else.

## src/syn.rs

Only change: `format_err!()` → `anyhow::anyhow!()`.

```
line 25:  format_err!("syn: missing null terminator at offset {}", pos)  → anyhow!("syn: missing null terminator at offset {}", pos)
line 32:  format_err!("syn: unexpected EOF reading word index")          → anyhow!("syn: unexpected EOF reading word index")
```

Add `use anyhow::anyhow;` at top.

## src/dictionary.rs

Only change: `format_err!()` → `anyhow::anyhow!()`.

```
line 24:  format_err!("bad dictionary directory {}", root.display())  → anyhow!("bad dictionary directory {}", root.display())
line 51:  format_err!("bad dictionary version {}", v)                 → anyhow!("bad dictionary version {}", v)
```

Add `use anyhow::anyhow;` at top.

---

## Verification

```sh
cargo test   # all 87 tests should still pass (after also fixing testdict.ifo synwordcount)
```

The `non_local_definitions` warnings will be gone.
