//! Wbudowuje silnik w wtyczkę: przy pierwszym starcie OBS wtyczka sama
//! instaluje go w %LOCALAPPDATA%\LiMBOCHAT\engine. Ścieżkę do zbudowanego
//! silnika podaje skrypt budujący w `LIMBOCHAT_ENGINE_EXE` (bez niej wtyczka
//! używa silnika, który już jest zainstalowany).

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LIMBOCHAT_ENGINE_EXE");
    println!("cargo:rerun-if-env-changed=LIMBOCHAT_VERSION");
    println!("cargo:rerun-if-changed=../obs-plugin/Cargo.toml");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("engine.bin");
    let engine = env::var("LIMBOCHAT_ENGINE_EXE").ok().filter(|p| !p.is_empty());
    match &engine {
        Some(path) => {
            println!("cargo:rerun-if-changed={path}");
            fs::copy(path, &out).unwrap_or_else(|e| panic!("nie mogę wczytać silnika {path}: {e}"));
        }
        None => fs::write(&out, b"").unwrap(),
    }
    // Wersja wbudowanego silnika = wersja pakietu obs-plugin (albo nadpisana w testach).
    let version = env::var("LIMBOCHAT_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| {
        let manifest = fs::read_to_string("../obs-plugin/Cargo.toml").unwrap_or_default();
        manifest
            .lines()
            .find_map(|l| l.trim().strip_prefix("version = \"").map(|v| v.trim_end_matches('"').to_owned()))
            .unwrap_or_else(|| "0.0.0".into())
    });
    println!("cargo:rustc-env=BUNDLED_ENGINE_VERSION={version}");
}
