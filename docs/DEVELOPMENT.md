# Rozwój i wydawanie LiMBOCHAT

## Mapa projektu

| Katalog | Odpowiedzialność |
| --- | --- |
| `src/` | Aplikacja egui i wspólne moduły Twitch, OAuth, Helix, 7TV oraz parser wiadomości |
| `obs-plugin/src/` | Silnik w osobnym procesie: serwer, LimboAI, OBS WebSocket, pamięć, podsumowania i aktualizacje |
| `obs-plugin/web/` | Panel przeglądarkowy i overlay |
| `obs-plugin/dla-znajomego/`, `obs-plugin/przegladarka/` | Pliki startowe paczek dla użytkowników |
| `obs-launcher/` | Mała biblioteka DLL uruchamiana przez OBS |
| `installer/` | Instalator osobnej aplikacji egui |
| `skrypty/` | Budowanie, pakowanie, publikowanie i testy PowerShell |
| `assets/` | Ikony, czcionki i zasoby aplikacji |
| `docs/img/` | Zrzuty i animacje do dokumentacji |
| `.github/` | CI i formularze zgłoszeń |

Pliki `.bat` w katalogu głównym są punktami wejścia dla użytkownika Windows. Implementacje skryptów znajdują się w `skrypty/`. `target/` i `wydania/` zawierają lokalne wyniki; nie trafiają do Gita. Moduły z `src/` są współdzielone przez silnik za pomocą `#[path]` — ich przeniesienie wymaga zmiany obu aplikacji.

## Środowisko i testy

Używaj aktualnego stabilnego Rusta, GitHub CLI oraz Visual Studio Build Tools z C++. Panel wymaga Node.js tylko do sprawdzenia składni; nie ma kroku npm ani bundlera.

```powershell
cargo test --locked -p limbochat-obs -p limbochat -p limbochat-obs-launcher
node --check obs-plugin/web/app.js
node --check obs-plugin/web/studio.js
cargo build --locked -p limbochat-obs --bin limbochat-engine
node tests/studio-e2e.mjs
powershell -NoProfile -ExecutionPolicy Bypass -File skrypty/testy.ps1
```

CI wykonuje te kontrole na Windows przy pushu do `main` i dla pull requestów. Test Studio wymaga Node.js 22 lub nowszego i używa lokalnych atrap OBS oraz dostawcy AI, z osobnym katalogiem danych. Nie łączy się z kontem Twitch, mikrofonem ani prawdziwym OBS i nie zużywa płatnego API.

IRC korzysta z systemowego TLS (`native-tls`; na Windows Schannel i magazyn certyfikatów systemu), aby nie wciągać przestarzałej gałęzi rustls z `twitch-irc` 5. HTTP i pozostałe WebSockety używają aktualnej gałęzi rustls z `Cargo.lock`. Na Linuxie kompilacja IRC wymaga bibliotek deweloperskich OpenSSL. Ręczny test samego połączenia TLS, bez logowania i wysyłania wiadomości: `cargo test --locked -p limbochat-obs --lib twitch_tls_handshake -- --ignored`.

Nie uruchamiaj `cargo test --workspace` przed zbudowaniem aplikacji: `installer/build.rs` pakuje gotowy plik wykonywalny. Instalator buduj przez `build-installer.bat`. Wtyczkę z wbudowanym silnikiem buduje `install-obs-plugin.bat`.

## Numery wersji

`obs-plugin/Cargo.toml` określa wersję silnika i aktualizacji dla OBS/panelu. Główny `Cargo.toml` dotyczy osobnej aplikacji egui. `obs-launcher` ma własną wersję stabilnej DLL. Te numery nie muszą być identyczne.

## Wydanie

1. Dopisz sprawdzone zmiany w `CHANGELOG.md` → „Następna wersja”.
2. Uruchom testy. Sprawdź funkcje wymagające prawdziwego Twitcha/OBS i zapisz ograniczenia.
3. Na komputerze autora uruchom `wydaj-automatycznie.bat` lub interaktywnie `wydaj-aktualizacje.bat`.

Skrypt buduje silnik i DLL, podpisuje aktualizację, tworzy paczki, wysyła źródła, a następnie tworzy wydanie z tagiem wskazującym zwrócony commit źródeł. Istniejący tag lub wydanie blokuje ponowne użycie numeru. Opis jest przesyłany przez plik UTF-8. Nie usuwamy automatycznie wydania po błędzie sieci — najpierw trzeba sprawdzić, czy GitHub zdążył je utworzyć.

Publikowanie źródeł przez API odwzorowuje pliki lokalnego projektu, z pominięciem katalogów wynikowych i sekretów. Dlatego zmiany z GitHuba (w tym `.github/`) muszą być obecne lokalnie przed uruchomieniem skryptu. Gdy folder jest klonem Gita (git z PATH albo z GitHub Desktop), skrypty pilnują tego same: wydanie wymaga gałęzi `main` bez niezapisanych plików i najpierw robi `git pull --ff-only`; wysłanie kodu zatrzymuje się, jeśli na GitHubie jest commit, którego nie ma w folderze; po wysłaniu folder jest przestawiany na opublikowany commit (`git reset` bez zmiany plików), więc zostaje czysty. Bez Gita skrypty działają jak dawniej.

Klucz podpisu zostaje w `%USERPROFILE%\.limbochat`; pliki `aktualizacje-*.txt` są lokalne. Nie generuj nowego klucza podczas porządkowania projektu — zainstalowane aplikacje ufają dotychczasowemu kluczowi publicznemu.
