# Mobile Bindings via uniffi-rs + Expo Module

## Overview

Two pieces:

1. `stardict-mobile/` — Rust crate using uniffi-rs that generates Swift and
   Kotlin bindings from our `stardict` library
2. `stardict-expo/` — Expo module that wraps the generated Swift/Kotlin
   bindings so they can be used from React Native via `expo-modules`

## Directory Structure

```
stardict-mobile/
  Cargo.toml
  src/lib.rs

stardict-expo/
  expo-module.config.json
  package.json
  src/
    index.ts                         # JS/TS API
    StarDictModule.ts                # typed bridge
  ios/
    StarDictModule.swift             # Expo module wrapping uniffi Swift
    StarDictModule.podspec           # CocoaPods spec linking staticlib
  android/
    build.gradle                     # links .so + generated Kotlin
    src/main/java/expo/modules/stardict/
      StarDictModule.kt              # Expo module wrapping uniffi Kotlin
```

## Part 1: uniffi-rs Crate

### 1. `stardict-mobile/Cargo.toml`

```toml
[package]
name = "stardict-mobile"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["lib", "cdylib", "staticlib"]
name = "stardict_mobile"

[dependencies]
uniffi = { version = "0.29", features = ["cli"] }
stardict = { path = ".." }

[build-dependencies]
uniffi = { version = "0.29", features = ["build"] }
```

### 2. `stardict-mobile/src/lib.rs`

```rust
use std::path::PathBuf;
use std::sync::Arc;

uniffi::setup_scaffolding!();

#[derive(uniffi::Record)]
pub struct DictEntry {
    pub entry_type: String,
    pub data: String,
}

#[derive(uniffi::Object)]
pub struct Dictionary {
    inner: stardict::dictionary::Dictionary,
}

#[uniffi::export]
impl Dictionary {
    #[uniffi::constructor]
    fn new(dir: String, name: String) -> Result<Arc<Self>, DictError> {
        let inner = stardict::dictionary::Dictionary::open(PathBuf::from(&dir), &name)
            .map_err(|e| DictError::LoadError { message: e.to_string() })?;
        Ok(Arc::new(Dictionary { inner }))
    }

    fn lookup(&self, word: String) -> Option<Vec<DictEntry>> {
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

    fn lookup_synonym(&self, word: String) -> Option<Vec<DictEntry>> {
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

    fn word_list(&self) -> Vec<String> {
        self.inner.word_list()
    }

    fn word_count(&self) -> u32 {
        self.inner.idx.entry_count() as u32
    }

    fn name(&self) -> String {
        self.inner.info().name.clone()
    }

    fn author(&self) -> String {
        self.inner.info().author.clone()
    }

    fn description(&self) -> String {
        self.inner.info().description.clone()
    }
}

#[derive(Debug, uniffi::Error)]
pub enum DictError {
    LoadError { message: String },
}

impl std::fmt::Display for DictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DictError::LoadError { message } => write!(f, "{}", message),
        }
    }
}
```

### 3. Build and generate bindings

```bash
cd stardict-mobile
cargo build --release

# Generate Swift
cargo run --features uniffi/cli --bin uniffi-bindgen generate \
  --library target/release/libstardict_mobile.dylib \
  --language swift \
  --out-dir ./bindings/swift

# Generate Kotlin
cargo run --features uniffi/cli --bin uniffi-bindgen generate \
  --library target/release/libstardict_mobile.dylib \
  --language kotlin \
  --out-dir ./bindings/kotlin
```

This produces:
- `bindings/swift/stardict_mobile.swift` + `stardict_mobileFFI.h` + `stardict_mobileFFI.modulemap`
- `bindings/kotlin/uniffi/stardict_mobile/stardict_mobile.kt`

## Part 2: Expo Module

### 4. `stardict-expo/package.json`

```json
{
  "name": "@stardict/expo",
  "version": "0.1.0",
  "main": "src/index.ts",
  "types": "src/index.ts",
  "scripts": {
    "build": "expo-module build"
  },
  "peerDependencies": {
    "expo": ">=52.0.0",
    "react": ">=18.0.0",
    "react-native": ">=0.76.0"
  },
  "devDependencies": {
    "expo": "^52.0.0",
    "expo-module-scripts": "^4.0.0"
  }
}
```

### 5. `stardict-expo/expo-module.config.json`

```json
{
  "platforms": ["ios", "android"],
  "ios": {
    "modules": ["StarDictModule"]
  },
  "android": {
    "modules": ["expo.modules.stardict.StarDictModule"]
  }
}
```

### 6. `stardict-expo/src/StarDictModule.ts`

```typescript
import { requireNativeModule } from 'expo-modules-core'

export interface DictEntry {
  entryType: string
  data: string
}

interface StarDictNativeModule {
  open(dir: string, name: string): number
  close(handle: number): void
  lookup(handle: number, word: string): DictEntry[] | null
  lookupSynonym(handle: number, word: string): DictEntry[] | null
  wordList(handle: number): string[]
  wordCount(handle: number): number
  getName(handle: number): string
  getAuthor(handle: number): string
  getDescription(handle: number): string
}

export default requireNativeModule<StarDictNativeModule>('StarDict')
```

### 7. `stardict-expo/src/index.ts`

```typescript
import StarDictModule, { type DictEntry } from './StarDictModule'

export type { DictEntry }

export class Dictionary {
  private handle: number

  constructor(dir: string, name: string) {
    this.handle = StarDictModule.open(dir, name)
  }

  close(): void {
    StarDictModule.close(this.handle)
  }

  lookup(word: string): DictEntry[] | null {
    return StarDictModule.lookup(this.handle, word)
  }

  lookupSynonym(word: string): DictEntry[] | null {
    return StarDictModule.lookupSynonym(this.handle, word)
  }

  wordList(): string[] {
    return StarDictModule.wordList(this.handle)
  }

  wordCount(): number {
    return StarDictModule.wordCount(this.handle)
  }

  get name(): string {
    return StarDictModule.getName(this.handle)
  }

  get author(): string {
    return StarDictModule.getAuthor(this.handle)
  }

  get description(): string {
    return StarDictModule.getDescription(this.handle)
  }
}
```

### 8. `stardict-expo/ios/StarDictModule.swift`

The Swift Expo module wraps the uniffi-generated `Dictionary` class. It uses
integer handles to hold `Dictionary` instances across JS calls.

```swift
import ExpoModulesCore

public class StarDictModule: Module {
    private var dictionaries: [Int: Dictionary] = [:]
    private var nextHandle = 1

    public func definition() -> ModuleDefinition {
        Name("StarDict")

        Function("open") { (dir: String, name: String) -> Int in
            let dict = try Dictionary(dir: dir, name: name)
            let handle = self.nextHandle
            self.nextHandle += 1
            self.dictionaries[handle] = dict
            return handle
        }

        Function("close") { (handle: Int) in
            self.dictionaries.removeValue(forKey: handle)
        }

        Function("lookup") { (handle: Int, word: String) -> [[String: String]]? in
            guard let dict = self.dictionaries[handle] else { return nil }
            guard let entries = dict.lookup(word: word) else { return nil }
            return entries.map { ["entryType": $0.entryType, "data": $0.data] }
        }

        Function("lookupSynonym") { (handle: Int, word: String) -> [[String: String]]? in
            guard let dict = self.dictionaries[handle] else { return nil }
            guard let entries = dict.lookupSynonym(word: word) else { return nil }
            return entries.map { ["entryType": $0.entryType, "data": $0.data] }
        }

        Function("wordList") { (handle: Int) -> [String] in
            guard let dict = self.dictionaries[handle] else { return [] }
            return dict.wordList()
        }

        Function("wordCount") { (handle: Int) -> Int in
            guard let dict = self.dictionaries[handle] else { return 0 }
            return Int(dict.wordCount())
        }

        Function("getName") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.name()
        }

        Function("getAuthor") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.author()
        }

        Function("getDescription") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.description()
        }
    }
}
```

### 9. `stardict-expo/ios/StarDictModule.podspec`

```ruby
require 'json'

package = JSON.parse(File.read(File.join(__dir__, '..', 'package.json')))

Pod::Spec.new do |s|
  s.name           = 'StarDictModule'
  s.version        = package['version']
  s.summary        = 'StarDict native module for Expo'
  s.homepage       = 'https://github.com/stardict'
  s.license        = 'MIT'
  s.author         = package['author']
  s.source         = { git: '' }

  s.platform       = :ios, '15.1'
  s.swift_version  = '5.9'
  s.source_files   = '*.swift'
  s.vendored_libraries = 'libstardict_mobile.a'
  s.preserve_paths = 'stardict_mobileFFI.h', 'stardict_mobileFFI.modulemap'

  s.pod_target_xcconfig = {
    'SWIFT_INCLUDE_PATHS' => '$(PODS_TARGET_SRCROOT)',
    'OTHER_LDFLAGS' => '-lstardict_mobile',
  }

  s.dependency 'ExpoModulesCore'
end
```

Copy into `stardict-expo/ios/`:
- `libstardict_mobile.a` (from `cargo build --release --target aarch64-apple-ios`)
- `stardict_mobileFFI.h` and `stardict_mobileFFI.modulemap` (from uniffi generate)
- `stardict_mobile.swift` (from uniffi generate)

### 10. `stardict-expo/android/build.gradle`

```groovy
apply plugin: 'com.android.library'
apply plugin: 'kotlin-android'
apply plugin: 'expo-module-scripts'

android {
  namespace 'expo.modules.stardict'
  compileSdk 35

  defaultConfig {
    minSdk 24
  }

  sourceSets {
    main {
      jniLibs.srcDirs = ['src/main/jniLibs']
      java.srcDirs = ['src/main/java']
    }
  }
}

dependencies {
  implementation project(':expo-modules-core')
  implementation 'net.java.dev.jna:jna:5.14.0@aar'
}
```

### 11. `stardict-expo/android/src/main/java/expo/modules/stardict/StarDictModule.kt`

```kotlin
package expo.modules.stardict

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import uniffi.stardict_mobile.Dictionary
import uniffi.stardict_mobile.DictEntry

class StarDictModule : Module() {
    private val dictionaries = mutableMapOf<Int, Dictionary>()
    private var nextHandle = 1

    override fun definition() = ModuleDefinition {
        Name("StarDict")

        Function("open") { dir: String, name: String ->
            val dict = Dictionary(dir, name)
            val handle = nextHandle++
            dictionaries[handle] = dict
            handle
        }

        Function("close") { handle: Int ->
            dictionaries.remove(handle)
        }

        Function("lookup") { handle: Int, word: String ->
            dictionaries[handle]?.lookup(word)?.map {
                mapOf("entryType" to it.entryType, "data" to it.data)
            }
        }

        Function("lookupSynonym") { handle: Int, word: String ->
            dictionaries[handle]?.lookupSynonym(word)?.map {
                mapOf("entryType" to it.entryType, "data" to it.data)
            }
        }

        Function("wordList") { handle: Int ->
            dictionaries[handle]?.wordList() ?: emptyList()
        }

        Function("wordCount") { handle: Int ->
            dictionaries[handle]?.wordCount()?.toInt() ?: 0
        }

        Function("getName") { handle: Int ->
            dictionaries[handle]?.name() ?: ""
        }

        Function("getAuthor") { handle: Int ->
            dictionaries[handle]?.author() ?: ""
        }

        Function("getDescription") { handle: Int ->
            dictionaries[handle]?.description() ?: ""
        }
    }
}
```

Copy into `stardict-expo/android/src/main/jniLibs/`:
- `arm64-v8a/libstardict_mobile.so` (from `cargo build --release --target aarch64-linux-android`)
- `armeabi-v7a/libstardict_mobile.so` (from `cargo build --release --target armv7-linux-androideabi`)
- `x86_64/libstardict_mobile.so` (from `cargo build --release --target x86_64-linux-android`)

Copy `stardict_mobile.kt` (from uniffi generate) into:
- `android/src/main/java/uniffi/stardict_mobile/`

### 12. No changes to existing stardict crate

## Usage in an Expo App

```typescript
import { Dictionary } from '@stardict/expo'

const dict = new Dictionary('/path/to/dict-dir', 'langdao-ce-gb')
console.log(dict.name)           // "朗道汉英字典5.0"
console.log(dict.wordCount())    // 405719

const result = dict.lookup('中国')
// [{ entryType: 'm', data: 'China\n...' }]

dict.close() // free native memory when done
```

## Build Pipeline Summary

```bash
# 1. Build Rust for all targets
cd stardict-mobile
cargo build --release                                    # macOS (for testing)
cargo build --release --target aarch64-apple-ios         # iOS
cargo build --release --target aarch64-linux-android     # Android arm64
cargo build --release --target armv7-linux-androideabi   # Android arm32
cargo build --release --target x86_64-linux-android      # Android x86_64

# 2. Generate bindings
cargo run --features uniffi/cli --bin uniffi-bindgen generate \
  --library target/release/libstardict_mobile.dylib \
  --language swift --out-dir ../stardict-expo/ios

cargo run --features uniffi/cli --bin uniffi-bindgen generate \
  --library target/release/libstardict_mobile.dylib \
  --language kotlin --out-dir ../stardict-expo/android/src/main/java

# 3. Copy compiled libraries
cp target/aarch64-apple-ios/release/libstardict_mobile.a ../stardict-expo/ios/

mkdir -p ../stardict-expo/android/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}
cp target/aarch64-linux-android/release/libstardict_mobile.so ../stardict-expo/android/src/main/jniLibs/arm64-v8a/
cp target/armv7-linux-androideabi/release/libstardict_mobile.so ../stardict-expo/android/src/main/jniLibs/armeabi-v7a/
cp target/x86_64-linux-android/release/libstardict_mobile.so ../stardict-expo/android/src/main/jniLibs/x86_64/
```
