//! Pakuje zbudowany `LiMBOCHAT.exe` do instalatora.
//!
//! Plik jest kompresowany (deflate) do `OUT_DIR/payload.bin`, a instalator
//! wbudowuje go przez `include_bytes!`. Dzięki temu znajomy dostaje jeden
//! plik, a sam instalator jest ~3× mniejszy od aplikacji.
//!
//! Ścieżkę do aplikacji można podać zmienną `LIMBOCHAT_EXE`; domyślnie
//! to `target/release/LiMBOCHAT.exe` w katalogu projektu.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().expect("installer/ leży w katalogu projektu").to_path_buf();

    println!("cargo:rerun-if-env-changed=LIMBOCHAT_EXE");
    let exe = env::var_os("LIMBOCHAT_EXE").map(PathBuf::from).unwrap_or_else(|| default_exe(&root));
    println!("cargo:rerun-if-changed={}", exe.display());

    let bytes = fs::read(&exe).unwrap_or_else(|e| {
        panic!(
            "\n\n  Nie znaleziono zbudowanej aplikacji: {} ({e})\n  \
             Najpierw zbuduj LiMBOCHAT:  cargo build --release\n  \
             albo po prostu uruchom:     build-installer.bat\n\n",
            exe.display()
        )
    });

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let file = fs::File::create(out.join("payload.bin")).expect("zapis payload.bin");
    let mut encoder = flate2::write::DeflateEncoder::new(file, flate2::Compression::best());
    encoder.write_all(&bytes).expect("kompresja aplikacji");
    encoder.finish().expect("kompresja aplikacji");
    println!("cargo:rustc-env=LIMBOCHAT_PAYLOAD_SIZE={}", bytes.len());

    // Wersja aplikacji z głównego Cargo.toml — pokazywana w instalatorze
    // i zapisywana w „Aplikacje i funkcje”.
    let cargo_toml = root.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", cargo_toml.display());
    let version = fs::read_to_string(&cargo_toml)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("version = \"")?.strip_suffix('"').map(str::to_owned))
        })
        .unwrap_or_else(|| "?".to_owned());
    println!("cargo:rustc-env=LIMBOCHAT_VERSION={version}");
}

fn default_exe(root: &Path) -> PathBuf {
    let dir = root.join("target").join("release");
    ["LiMBOCHAT.exe", "LiMBOCHAT"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| dir.join("LiMBOCHAT.exe"))
}
