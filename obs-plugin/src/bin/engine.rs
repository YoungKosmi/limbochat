//! `limbochat-engine` — silnik LiMBOCHAT uruchamiany przez wtyczkę OBS.
//!
//! Program konsolowy, ale wtyczka uruchamia go bez okna (CREATE_NO_WINDOW),
//! więc nic nie wyskakuje. Dzięki temu skrypty wydawania aktualizacji mogą
//! normalnie czytać jego wyjście (`--keygen`, `--sign`, `--selftest`).

fn main() {
    std::process::exit(limbochat_engine::engine::main());
}
