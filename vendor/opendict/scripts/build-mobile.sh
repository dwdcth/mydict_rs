#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MOBILE="$ROOT/mobile"
EXPO="$ROOT/expo"

ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
# Pick the latest NDK
NDK_VERSION=$(ls "$ANDROID_HOME/ndk/" 2>/dev/null | sort -V | tail -1)
ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$NDK_VERSION"

echo "=== opendict-rs mobile build ==="
echo "Root:         $ROOT"
echo "Android NDK:  $ANDROID_NDK_HOME"
echo ""

# ── 1. Build for host (needed for uniffi-bindgen metadata extraction) ──

echo "▸ Building mobile crate for host..."
cd "$MOBILE"
cargo build --release --quiet

# ── 2. Generate Swift + Kotlin bindings ──

echo "▸ Generating Swift bindings..."
BINDGEN_OUT="$MOBILE/target/uniffi-swift"
rm -rf "$BINDGEN_OUT"
mkdir -p "$BINDGEN_OUT"
cargo run --release --quiet --bin uniffi-bindgen -- generate \
  --library target/release/libopendict_mobile.dylib \
  --language swift \
  --out-dir "$BINDGEN_OUT"

# uniffi-bindgen names the modulemap <CrateName>FFI.modulemap. Rename so it
# slots into the XCFramework's Modules dir as the canonical "module.modulemap".
mv "$BINDGEN_OUT/opendict_mobileFFI.modulemap" "$BINDGEN_OUT/module.modulemap"

# Copy the Swift wrapper to the pod source dir; the .h and .modulemap live in the XCFramework.
cp "$BINDGEN_OUT/opendict_mobile.swift" "$EXPO/ios/opendict_mobile.swift"

echo "▸ Generating Kotlin bindings..."
cargo run --release --quiet --bin uniffi-bindgen -- generate \
  --library target/release/libopendict_mobile.dylib \
  --language kotlin \
  --out-dir "$EXPO/android/src/main/java"

# ── 3. Build iOS static libs (device + simulator) ──

echo "▸ Building for aarch64-apple-ios..."
cargo build --release --quiet --target aarch64-apple-ios

echo "▸ Building for aarch64-apple-ios-sim..."
cargo build --release --quiet --target aarch64-apple-ios-sim

# ── 4. Assemble XCFramework ──

echo "▸ Creating XCFramework..."
XCF="$EXPO/ios/opendict_mobile.xcframework"
rm -rf "$XCF"

# Each -library slice gets a private headers dir containing both the FFI header
# and the modulemap. xcodebuild copies them into the slice's Headers/ + Modules/.
HEADERS_STAGE="$MOBILE/target/uniffi-headers"
rm -rf "$HEADERS_STAGE"
mkdir -p "$HEADERS_STAGE"
cp "$BINDGEN_OUT/opendict_mobileFFI.h" "$HEADERS_STAGE/"
cp "$BINDGEN_OUT/module.modulemap" "$HEADERS_STAGE/"

xcodebuild -create-xcframework \
  -library "$MOBILE/target/aarch64-apple-ios/release/libopendict_mobile.a" \
  -headers "$HEADERS_STAGE" \
  -library "$MOBILE/target/aarch64-apple-ios-sim/release/libopendict_mobile.a" \
  -headers "$HEADERS_STAGE" \
  -output "$XCF" >/dev/null

# ── 5. Build Android shared libs (all four ABIs) ──

export ANDROID_NDK_HOME

# Map cargo target → Android ABI directory name
declare -a ANDROID_TARGETS=(
  "aarch64-linux-android:arm64-v8a"
  "armv7-linux-androideabi:armeabi-v7a"
  "i686-linux-android:x86"
  "x86_64-linux-android:x86_64"
)

for entry in "${ANDROID_TARGETS[@]}"; do
  rust_target="${entry%%:*}"
  abi="${entry##*:}"
  echo "▸ Building for $rust_target..."
  cargo ndk --target "$rust_target" --platform 24 -- build --release --quiet
  mkdir -p "$EXPO/android/src/main/jniLibs/$abi"
  cp "$MOBILE/target/$rust_target/release/libopendict_mobile.so" \
     "$EXPO/android/src/main/jniLibs/$abi/"
done

# ── 6. Build the Expo module (TypeScript → build/) ──

echo "▸ Building Expo module (TypeScript)..."
cd "$EXPO"
if [ ! -d node_modules ]; then
  npm install --silent
fi
# expo-module build adds --watch when stdout is a TTY; force one-shot
EXPO_NONINTERACTIVE=1 npm run --silent build

# ── Done ──

echo ""
echo "✓ iOS:     $XCF"
echo "✓ Android: $EXPO/android/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86,x86_64}/libopendict_mobile.so"
echo "✓ Swift:   $EXPO/ios/opendict_mobile.swift"
echo "✓ Kotlin:  $EXPO/android/src/main/java/uniffi/opendict_mobile/"
echo "✓ JS:      $EXPO/build/index.js"
echo ""
echo "Done. Run your Expo app with: cd examples/expo-test && npx expo run:ios"
