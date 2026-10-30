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
powershell -NoProfile -ExecutionPolicy Bypass -File skrypty/testy.ps1
```

CI wykonuje te kontrole na Windows przy pushu do `main` i dla pull requestów. Nie łączy się z kontem Twitch, mikrofonem ani OBS.

Nie uruchamiaj `cargo test --workspace` przed zbudowaniem aplikacji: `installer/build.rs` pakuje gotowy plik wykonywalny. Instalator buduj przez `build-installer.bat`. Wtyczkę z wbudowanym silnikiem buduje `install-obs-plugin.bat`.

## Numery wersji

`obs-plugin/Cargo.toml` określa wersję silnika i aktualizacji dla OBS/panelu. Główny `Cargo.toml` dotyczy osobnej aplikacji egui. `obs-launcher` ma własną wersję stabilnej DLL. Te numery nie muszą być identyczne.

## Wydanie

1. Dopisz sprawdzone zmiany w `CHANGELOG.md` → „Następna wersja”.
2. Uruchom testy. Sprawdź funkcje wymagające prawdziwego Twitcha/OBS i zapisz ograniczenia.
3. Na komputerze autora uruchom `wydaj-automatycznie.bat` lub interaktywnie `wydaj-aktualizacje.bat`.

Skrypt buduje silnik i DLL, podpisuje aktualizację, tworzy paczki, wysyła źródła, a następnie tworzy wydanie z tagiem wskazującym zwrócony commit źródeł. Istniejący tag lub wydanie blokuje ponowne użycie numeru. Opis jest przesyłany przez plik UTF-8. Nie usuwamy automatycznie wydania po błędzie sieci — najpierw trzeba sprawdzić, czy GitHub zdążył je utworzyć.

Publikowanie źródeł przez API odwzorowuje pliki lokalnego projektu, z pominięciem katalogów wynikowych i sekretów. Dlatego zmiany z GitHuba (w tym `.github/`) muszą być obecne lokalnie przed uruchomieniem skryptu. Przy pracy w klonie sprawdź `git status` i synchronizację z `main`.

Klucz podpisu zostaje w `%USERPROFILE%\.limbochat`; pliki `aktualizacje-*.txt` są lokalne. Nie generuj nowego klucza podczas porządkowania projektu — zainstalowane aplikacje ufają dotychczasowemu kluczowi publicznemu.
