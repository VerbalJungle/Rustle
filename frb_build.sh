#!/usr/bin/env bash
set -e

echo "1/3 Generiere Flutter-Rust-Bridge Bindings..."
flutter_rust_bridge_codegen generate

echo "2/3 Kompiliere Rust-Core für Linux..."
cargo build --manifest-path rust/Cargo.toml

echo "3/3 Bereinige gecachte .so-Libraries im Flutter-Linux-Build..."
rm -f build/linux/x64/debug/bundle/lib/librust_lib_rustle.so
rm -f build/linux/x64/release/bundle/lib/librust_lib_rustle.so
find build/ -name "librust_lib_rustle.so" -delete 2>/dev/null || true
flutter gen-l10n

# Falls noch ein altes Hotspot-Profil in NetworkManager hängt, vorsorglich löschen:
nmcli connection delete Rust_Hotspot 2>/dev/null || true

echo "Fertig! Bitte jetzt die Linux-/Android-App neu starten (Stop + Run)."
