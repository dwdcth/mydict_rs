# Publishing plan

This repo ships three artifacts — the Rust crate, the Node.js package,
and the Expo package. They all wrap the same core (`opendict-rs`).

| Artifact            | Registry  | Source dir | Status                          |
| ------------------- | --------- | ---------- | ------------------------------- |
| `opendict-rs`       | crates.io | `/`        | metadata complete; ready          |
| `@opendict-rs/node` | npm       | `node/`    | scaffolded; needs CI build matrix |
| `@opendict-rs/expo` | npm       | `expo/`    | ready; CI workflow set up         |

The plan below is sequential — finish one before starting the next.
Everything assumes manual publishing for v0.1.x and CI-driven releases
for v0.2+.

## 1. Rust crate (`opendict-rs`)

### Pre-publish checklist
- [ ] Add a `LICENSE` file at the repo root (MIT, matching `Cargo.toml`)
- [ ] Verify `Cargo.toml` metadata: `description`, `homepage`, `repository`, `license`, `keywords`, `categories`
- [ ] Update `homepage` and `repository` URLs once the GitHub repo is final
- [ ] `cargo doc --no-deps --open` and skim the rustdoc for missing/wrong docstrings
- [ ] `cargo test` and `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo publish --dry-run` to surface any issues
- [ ] Tag `v0.1.0` and `cargo publish`

### Versioning
- Follow semver. `0.1.x` for early development.
- The Rust crate is the source of truth — bumping it usually triggers bumps in node + expo.

### What to do once published
- The `opendict-rs` name on crates.io is locked to this repo. Subsequent
  versions only need `cargo publish` after a version bump.

## 2. Expo package (`@opendict-rs/expo`)

The build pipeline (`scripts/build-mobile.sh`) already produces a valid
tarball. The remaining work is mostly polish + extra Android ABIs.

### Pre-publish checklist
- [ ] Pick a final scope/name. Currently `@opendict-rs/expo`. Either claim
      the `opendict` org on npm or rename to e.g. `opendict-expo`.
- [ ] Fill in `expo/package.json` properly:
      `description`, `repository`, `homepage`, `bugs`, `keywords`, `author`, `license`
- [ ] Write `expo/README.md` documenting installation and usage
- [ ] Add a `prepublishOnly` script in `expo/package.json`:
      `"prepublishOnly": "../scripts/build-mobile.sh"` so a fresh publish
      always rebuilds bindings + native libs from source
- [ ] Extend `scripts/build-mobile.sh` to build all four Android ABIs:
      `aarch64-linux-android` (already), `armv7-linux-androideabi`,
      `i686-linux-android`, `x86_64-linux-android`. Drop each into the
      matching `jniLibs/<abi>/` directory.
- [ ] Verify `expo/package.json` `files` array still matches what's on disk
      after the build (xcframework, jniLibs, build/, src/, etc.)
- [ ] `npm pack` and inspect the tarball — confirm ~size, no node_modules,
      no `.tgz` recursion, no source maps if you don't want them
- [ ] Smoke-test the tarball end-to-end via `examples/expo-test/` on both
      iOS simulator and Android emulator
- [ ] Tag `expo-v0.1.0` and `npm publish` from `expo/`

### iOS architecture coverage
- The XCFramework currently includes `ios-arm64` (device) and
  `ios-arm64-simulator` (Apple Silicon Macs). That's enough for shipped
  apps and modern dev machines.
- If users on Intel Macs need to build, add `x86_64-apple-ios` and merge
  it into the simulator slice via `lipo`.

### Versioning
- Independent semver. Bump the package version when the package itself
  changes (binding API, build pipeline, etc.).
- When the underlying Rust core changes shape, bump expo accordingly.

## 3. Node package (`opendict`)

The biggest piece of work. napi-rs has a multi-package release pattern;
follow it rather than rolling your own.

### The napi-rs multi-package model
- Main package `opendict` ships only JS dispatcher + types + `index.js`
  that selects a binary based on `process.platform` + `process.arch`.
- One package per platform — names are conventional:
  `@opendict/darwin-arm64`, `@opendict/darwin-x64`,
  `@opendict/linux-x64-gnu`, `@opendict/linux-arm64-gnu`,
  `@opendict/linux-arm64-musl`, `@opendict/win32-x64-msvc`, etc.
- Each platform package contains exactly one `.node` binary plus a tiny
  `package.json`.
- The main package lists every platform package under
  `optionalDependencies` with the same version. npm only installs the
  ones whose `os`/`cpu` constraints match the current machine.

### Pre-publish checklist
- [ ] `npm install -g @napi-rs/cli@latest` if not already
- [ ] Update `node/package.json`:
      proper `name` (likely `opendict`), `description`, `repository`,
      `keywords`, `author`, `license`
- [ ] Configure `napi.targets` in `node/package.json` listing every
      target you intend to ship (start small — Apple Silicon + Linux x64
      covers most CI users)
- [ ] Run `napi create-npm-dirs` to scaffold per-platform directories
      under `node/npm/<target>/`
- [ ] Write a GitHub Actions workflow that:
      - Builds the binary on each target (matrix of `os` + `target`)
      - Runs `napi build --platform --release --target <triple>`
      - Uploads each `.node` artifact
      - On a release tag, downloads all artifacts, places them under
        `npm/<target>/`, and runs `napi prepublish` then `npm publish` for
        the main package and each platform package
      - napi-rs has an official template: copy from
        https://github.com/napi-rs/package-template
- [ ] Smoke-test by installing the published `opendict` in a fresh
      project on each target platform — it should pull only the matching
      platform package
- [ ] Tag `node-v0.1.0` and trigger the release workflow

### Targets to ship at v0.1.0 (minimum useful set)
- `aarch64-apple-darwin` (Apple Silicon Macs)
- `x86_64-apple-darwin` (Intel Macs)
- `x86_64-unknown-linux-gnu` (most Linux CI / servers)
- `aarch64-unknown-linux-gnu` (ARM Linux servers)

Add Windows + musl + linux-arm64 once the workflow is solid.

### Versioning
- Independent semver from the Rust crate but conceptually tracking it.
- Bump when the Rust API the addon exposes changes, or when the addon's
  JS surface changes.

## Cross-cutting

### Repository hygiene
- [ ] Confirm `LICENSE` file exists at the repo root and is MIT
- [ ] Confirm the GitHub repo URL is the same one referenced in every
      `package.json` / `Cargo.toml` (`callum/opendict-rs` currently)
- [ ] Add a top-level `CHANGELOG.md` and follow Keep-a-Changelog format
- [ ] Add a `.github/` dir with at least a release workflow + issue templates

### GitHub Actions workflows (set up alongside manual publish)

Three workflows in `.github/workflows/`, each tag-triggered. Each one
encodes the pre-publish checklist for its artifact, so once they're
green the registry publish is the obvious next step. While we still
publish manually for v0.1.x, having the workflows means the next
release can flip to fully automated by adding a publish step at the
end of each.

- `crate-release.yml` — fires on `crate-v*`. Runs `cargo test`,
  `cargo clippy`, `cargo publish --dry-run`. Add `cargo publish` once
  you have a `CARGO_REGISTRY_TOKEN` secret.
- `expo-release.yml` — fires on `expo-v*`. Runs on macOS (needed for
  iOS XCFramework). Steps: install Rust + Android NDK,
  `bash scripts/build-mobile.sh`, `cd expo && npm pack`, upload tarball
  as a release artifact. Add `npm publish` once you have an `NPM_TOKEN`.
- `node-release.yml` — fires on `node-v*`. Matrix build across all
  napi-rs targets (Linux x64/arm64 (gnu+musl), macOS x64/arm64,
  Windows x64). Each builds `napi build --release --target <triple>`
  and uploads its `.node`. A final job downloads all artifacts and
  publishes the main package + each platform package.

A shared `ci.yml` (push to master + PRs) runs `cargo test` + the node
test suite + the expo TypeScript build, so regressions land before a
release tag does.

### Smoke tests before each release
- Rust: `cargo test` + run the criterion benches once
- Node: `examples/node-test/` runs all assertions against fixtures
- Expo: `examples/expo-test/` on iOS sim + Android emulator with
  `setup-dicts.sh && push-dicts.sh` exercising real-world dictionaries

## Order of work

1. Rust crate first (~30 min, smallest blast radius)
2. Expo package next (extra Android ABIs + metadata + README + manual publish)
3. Node package last (multi-package + CI matrix is the largest piece)
