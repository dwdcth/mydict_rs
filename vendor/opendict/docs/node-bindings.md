# Node Bindings via napi-rs

## Overview

A separate crate `stardict-node/` that wraps our `stardict` library as a native
Node.js addon using napi-rs. It exposes a `Dictionary` class that can be used
from JavaScript/TypeScript.

## Directory Structure

```
stardict-node/
  Cargo.toml
  build.rs
  package.json
  src/lib.rs
```

## API

```typescript
class Dictionary {
  constructor(dir: string, name: string)
  lookup(word: string): Array<{ type: string, data: string }> | null
  lookupSynonym(word: string): Array<{ type: string, data: string }> | null
  wordList(): Array<string>
  wordCount(): number
  readonly name: string
  readonly wordcount: number
  readonly author: string
  readonly description: string
}
```

## File Changes

### 1. `stardict-node/Cargo.toml`

```toml
[package]
name = "stardict-node"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
napi = "3"
napi-derive = "3"
stardict = { path = ".." }

[build-dependencies]
napi-build = "1"
```

### 2. `stardict-node/build.rs`

```rust
extern crate napi_build;

fn main() {
    napi_build::setup();
}
```

### 3. `stardict-node/package.json`

```json
{
  "name": "stardict",
  "version": "0.1.0",
  "main": "index.js",
  "types": "index.d.ts",
  "napi": {
    "name": "stardict",
    "triples": {}
  },
  "scripts": {
    "build": "napi build --platform --release",
    "build:debug": "napi build --platform"
  },
  "devDependencies": {
    "@napi-rs/cli": "^3"
  }
}
```

### 4. `stardict-node/src/lib.rs`

```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;

use std::path::PathBuf;

#[napi(object)]
pub struct DictEntry {
    pub entry_type: String,
    pub data: String,
}

#[napi]
pub struct Dictionary {
    inner: stardict::dictionary::Dictionary,
}

#[napi]
impl Dictionary {
    #[napi(constructor)]
    pub fn new(dir: String, name: String) -> Result<Self> {
        let inner = stardict::dictionary::Dictionary::open(PathBuf::from(&dir), &name)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Dictionary { inner })
    }

    #[napi]
    pub fn lookup(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
    }

    #[napi]
    pub fn lookup_synonym(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup_synonym(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
    }

    #[napi]
    pub fn word_list(&self) -> Vec<String> {
        self.inner.word_list()
    }

    #[napi]
    pub fn word_count(&self) -> u32 {
        self.inner.idx.entry_count() as u32
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.info().name.clone()
    }

    #[napi(getter)]
    pub fn author(&self) -> String {
        self.inner.info().author.clone()
    }

    #[napi(getter)]
    pub fn description(&self) -> String {
        self.inner.info().description.clone()
    }
}
```

### 5. No changes to existing stardict crate

The node bindings depend on the existing public API. Nothing in `src/` changes.

## Build & Usage

```bash
cd stardict-node
npm install
npm run build
```

```javascript
const { Dictionary } = require('./index.js')

const dict = new Dictionary('/path/to/stardict-langdao-ce-gb-2.4.2', 'langdao-ce-gb')
console.log(dict.name)        // "朗道汉英字典5.0"
console.log(dict.wordCount()) // 405719

const result = dict.lookup('中国')
// [{ entryType: 'm', data: '...' }]
```

## Generated TypeScript types

Running `napi build` auto-generates `index.d.ts` and `index.js`. The types
will look like:

```typescript
export interface DictEntry {
  entryType: string
  data: string
}

export declare class Dictionary {
  constructor(dir: string, name: string)
  lookup(word: string): Array<DictEntry> | null
  lookupSynonym(word: string): Array<DictEntry> | null
  wordList(): Array<string>
  wordCount(): number
  get name(): string
  get author(): string
  get description(): string
}
```
