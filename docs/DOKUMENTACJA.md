# LiMBOCHAT — pełna dokumentacja

Szczegóły wszystkich funkcji, budowania i wydawania. Krótkie wprowadzenie jest w [README](../README.md).

## Funkcje

- Wiele kanałów naraz w zakładkach, z licznikiem nieprzeczytanych wiadomości i podświetleniem wzmianek
- Pisanie na czacie, w tym emotki 7TV: picker 😀 albo podpowiedzi klawiszem **Tab** (`@` + Tab uzupełnia nicki)
- Emotki 7TV (globalne i kanałowe, animowane, zero-width) oraz odznaki Twitcha w linii z tekstem
- Moderacja jednym kliknięciem (usuń, timeout, ban z Shiftem) i pełne menu pod prawym przyciskiem na nicku
- Logowanie przez Twitch (Device Code Flow) z awatarem; token przechowywany w systemowym sejfie

## Wygląd

Motyw „liquid glass” w matowej palecie fiolet–czerń–biel: szklane panele nad płynącą zorzą, animowane przejścia (wjazd wiadomości, pigułka aktywnej zakładki, pulsujące wskaźniki, słupki aktywności czatu). Animacje działają tylko przy aktywnym oknie i można je wyłączyć w menu ⚙.

W menu ⚙ jest też opcja **Szkło systemowe (Windows 11)**: system rozmywa pulpit pod oknem (efekt Mica). Wymaga ponownego uruchomienia.

Czcionki: Inter, Outfit i JetBrains Mono na licencji SIL Open Font License (`assets/fonts/OFL.txt`).

## Uruchomienie

Wymagany Rust (https://rustup.rs), a na Windowsie także Visual Studio Build Tools z pakietem C++.

```
cargo run --release
```

Plik wykonywalny trafia do `target/release/LiMBOCHAT.exe`. Można go skopiować w dowolne miejsce.

Tryb demo bez sieci: `set TMC_DEMO=1 && cargo run` (cmd) lub `$env:TMC_DEMO=1; cargo run` (PowerShell).

## Instalator dla znajomych

Dwuklik na `build-installer.bat` buduje aplikację i jednoplikowy instalator **`LiMBOCHAT-Setup.exe`** (pojawi się w folderze projektu). Ten jeden plik wystarczy wysłać — znajomy nie potrzebuje Rusta ani niczego innego.

Instalator:
- instaluje dla bieżącego użytkownika (`%LOCALAPPDATA%\Programs\LiMBOCHAT`), bez uprawnień administratora,
- tworzy skrót w menu Start i opcjonalnie na pulpicie,
- dodaje wpis w „Aplikacje i funkcje” (stamtąd można odinstalować),
- uruchomiony ponownie aktualizuje aplikację, zachowując ustawienia i logowanie.

Każdy loguje się własnym kontem Twitch — tokeny są zapisywane tylko na jego komputerze.

Plik nie jest podpisany certyfikatem, więc Windows SmartScreen pokaże ostrzeżenie: **Więcej informacji → Uruchom mimo to**.

Cicha instalacja (bez okna): `LiMBOCHAT-Setup.exe --silent`.

## Wtyczka do OBS (panel / dock)

LiMBOCHAT działa też jako panel w OBS Studio (28 lub nowszy) — te same funkcje co aplikacja: kilka kanałów w zakładkach, pisanie z emotkami 7TV, moderacja, logowanie przez Twitch, a do tego LimboAI i overlay na stream.

**Instalacja:** zamknij OBS i kliknij dwa razy `install-obs-plugin.bat`.

**Dodanie panelu (jednorazowo):** w OBS menu **Doki → Niestandardowe doki przeglądarki…**, nazwa `LiMBOCHAT`, adres `http://127.0.0.1:47250`, **Zastosuj**.

### Jak to działa

```
OBS ── limbochat.dll (mała wtyczka, prawie się nie zmienia)
          │ uruchamia przy starcie OBS / zatrzymuje przy zamknięciu
          ▼
       limbochat-engine.exe  (%LOCALAPPDATA%\LiMBOCHAT\engine)
          czat · 7TV · moderacja · LimboAI · serwer 127.0.0.1:47250
          ▲
          └── autoaktualizacje z GitHub Releases (podpisane)
```

- Silnik to **osobny proces**: może się zaktualizować w trakcie streama, a OBS działa dalej. Awaria silnika nie wyłoży OBS.
- Serwer słucha tylko na 127.0.0.1 i odrzuca połączenia z innych stron (Host/Origin), więc żadna strona w przeglądarce nie wyśle komendy w Twoim imieniu.
- Silnik kończy pracę razem z OBS (także po awarii OBS).

Logowanie w panelu OBS jest osobne od aplikacji PC. Ustawienia: `%APPDATA%\LiMBOCHAT\obs-dock.json`, logi: `%APPDATA%\LiMBOCHAT\engine.log` i `obs-plugin.log`. Tryb demo (bez Twitcha): uruchom OBS ze zmienną `LIMBOCHAT_DEMO=1`.

Odinstalowanie: usuń `C:\ProgramData\obs-studio\plugins\limbochat` i `%LOCALAPPDATA%\LiMBOCHAT\engine`.

### Autoaktualizacje (bez restartu OBS)

**Raz:**
1. Załóż konto na GitHubie i (opcjonalnie) zainstaluj [GitHub CLI](https://cli.github.com), potem w terminalu `gh auth login`.
2. Kliknij `ustaw-aktualizacje.bat` i wpisz `nick/nazwa` repozytorium (np. `kosmi/limbochat`). Skrypt utworzy klucz podpisu w `%USERPROFILE%\.limbochat\klucz-aktualizacji.key` i (z GitHub CLI) publiczne repozytorium.
3. Zamknij OBS → `install-obs-plugin.bat`, a znajomym wyślij **ostatni raz** nową paczkę: `pakiet-dla-znajomego.bat`.

**Każda nowa wersja:** `wydaj-aktualizacje.bat` → wpisz numer wersji i jedno zdanie „co nowego”. Skrypt zbuduje silnik, podpisze go i wrzuci wydanie na GitHuba (bez GitHub CLI otworzy stronę wydania i folder z 3 plikami do przeciągnięcia).

**U Ciebie i znajomych:** silnik sprawdza GitHuba co 20 minut (albo ⚙ → Aktualizacje → *Sprawdź teraz*), pobiera nową wersję i:
- sprawdza **podpis** (Ed25519: wersja + nazwa pliku + SHA-256) — plik bez Twojego podpisu nie zostanie użyty, nawet gdyby ktoś przejął konto GitHub,
- sprawdza, czy nowa wersja się uruchamia,
- gdy LimboAI nic nie robi (tryb *Automatycznie*) albo po kliknięciu (tryb *Pytaj mnie*) przekazuje jej **czat** i port — przerwa trwa ułamek sekundy, panel i overlay przeładowują się same, OBS działa dalej,
- jeśli nowa wersja nie wstanie, stara wraca do pracy i tej wersji już nie próbuje.

**Klucz podpisu** (`klucz-aktualizacji.key`) zrób w kopii (np. pendrive) i nigdy go nikomu nie wysyłaj. Bez niego nie wydasz aktualizacji dla już zainstalowanych kopii (trzeba by rozesłać nową paczkę). Repozytorium musi być publiczne (silnik pobiera wydania bez logowania); wystarczą w nim same wydania, kod może zostać u Ciebie.

Zmiana samej wtyczki `limbochat.dll` (rzadko) nadal wymaga restartu OBS — dlatego jest malutka i nic w niej nie zmieniamy.

**Wtyczka dla znajomych:** `pakiet-dla-znajomego.bat` tworzy `LiMBOCHAT-OBS-dla-znajomego.zip` (`limbochat.dll` z wbudowanym silnikiem + `ZAINSTALUJ.bat` + `INSTRUKCJA.txt`). Każde wydanie na GitHubie zawiera też gotowy `LiMBOCHAT-OBS.zip` dla nowych osób. Każdy loguje się własnym kontem Twitch i wkleja własne klucze API.

## Alerty na czacie (wtyczka OBS)

Bitsy, suby, resuby, gifty, gift-bomby, raidy, odznaki bitsów, follow i nagrody za punkty kanału pojawiają się na czacie jako **animowane karty**: wybuch cząsteczek, liczenie kwoty w górę, tęczowa ramka i konfetti przy dużych alertach, kolor bitsów jak na Twitchu (100 / 1000 / 5000 / 10 000). Pojedyncze gifty z gift-bomby dopisują się do jednej karty („Dla: …”), zamiast zalewać czat.

**Rymowanki LimboAI:** do każdego alertu LimboAI układa krótką, życzliwą rymowankę z nickiem widza (Gemini albo własny model). Opcje w ⚙ → *Alerty na czacie*: włącz/wyłącz rymowanki, czytanie na głos, wysyłanie na czat (jako Ty), follow, nagrody, „pierwszy raz na czacie”, dźwięk. Przycisk *Pokaż przykład* pokazuje każdy rodzaj alertu bez czekania.

Bitsy, suby, gifty i raidy przychodzą z czatu (dowolny kanał). **Follow i nagrody za punkty** przychodzą z Twitch EventSub i działają tylko na Twoim kanale — po aktualizacji **zaloguj się ponownie** (nowe uprawnienia `moderator:read:followers`, `channel:read:redemptions`). Nagrody za punkty mają tylko afilianci i partnerzy.

## Obrazki z kappa.lol

Link z kappa.lol (także bez `https://`, np. `kappa.lol/AbCd.png`) od razu pokazuje się na czacie jako obrazek albo krótkie wideo; kliknięcie powiększa, „Otwórz w przeglądarce” otwiera stronę. Obrazki pobiera silnik (nie przeglądarka panelu): tylko z kappa.lol, tylko https, tylko png/jpg/gif/webp/avif/mp4/webm do 25 MB, z pamięcią podręczną — Twój adres IP nie trafia do serwisu. W ⚙ wybierasz, od kogo pokazywać: wszyscy / sub, VIP, mod / wyłączone. Usunięta wiadomość rozmywa obrazek.

## LimboAI — asystent głosowy (wtyczka OBS)

Naciśnij **Shift + Z + X** (działa też w grze), powiedz polecenie, np. „zmień kategorię na GTA 4” — LimboAI wykona je i odpowie głosem. Okno LimboAI pojawia się u góry panelu czatu.

**Jeden przycisk do mówienia:** ⚙ → *Przycisk do mówienia* → *Zmień* i naciśnij jeden klawisz (F1–F24, klawiatura numeryczna, Pause, Insert, Home, End, PgUp/PgDn) albo boczny/środkowy przycisk myszy. Sama litera nie jest dozwolona — przeszkadzałaby w pisaniu. Tryby:
- *Naciśnij i mów* — naciśnij, mów, naciśnij ponownie (albo zamilknij),
- *Przytrzymaj i mów* (push-to-talk) — trzymaj przycisk, gdy mówisz; po puszczeniu LimboAI działa. Pauzy w środku zdania nie przerywają nagrania.

Klawisze F13–F24 nie istnieją na zwykłej klawiaturze, ale można je przypisać w programie myszy, Stream Decku albo klawiatury makro — idealne do mówienia, bo nie kolidują z grami.

**Konfiguracja (jednorazowo):** w panelu ⚙ → *LimboAI*:
- **Groq** (zalecany, rozpoznawanie mowy, darmowy limit) — [Groq Console](https://console.groq.com/keys). Bez niego mowę rozpoznaje Gemini.
- **Mózg (AI)** — do wyboru:
  - **Gemini** — klucz z [Google AI Studio](https://aistudio.google.com/apikey),
  - **Własny model** (dowolne API zgodne z OpenAI): OpenAI, OpenRouter (setki modeli), Groq, **Ollama** i **LM Studio** (lokalnie, za darmo, bez klucza) albo własny adres. Wybierz dostawcę, kliknij *Pobierz listę* i wybierz model. Model musi obsługiwać narzędzia (function calling) — np. `gpt-4.1-mini`, `llama-3.3-70b-versatile` (Groq), `qwen2.5:7b` / `llama3.1:8b` (Ollama). Małe modele lokalne działają, ale częściej się mylą.

Klucze trafiają do sejfu Windows (Menedżer poświadczeń), nie do pliku ustawień. Własny adres musi być `https://` (albo `http://localhost` dla modeli lokalnych), żeby klucz nie leciał otwartym tekstem. Tam też zmienisz mikrofon, model i głos.

**Overlay na stream:** w OBS *Źródła → + → Przeglądarka*, adres `http://127.0.0.1:47250/overlay`, rozmiar 800 × 300. Widzowie zobaczą, kiedy LimboAI słucha, Twoje polecenie, wykonane akcje i odpowiedź (karta sama się chowa). W ⚙ → *Overlay na stream*: podgląd, pozycja, rozmiar, ukrywanie polecenia i akcji. Overlay dostaje tylko zdarzenia LimboAI i kolor — nie widzi czatu ani ustawień, a treść błędów nie trafia na stream.

**„Hej Limbo” (bez klawiszy):** ⚙ → *Naucz mój głos* → powiedz 3× „Hej Limbo”. Potem wystarczy „Hej Limbo, zmień kategorię na GTA 4” — także w jednym zdaniu. Wykrywanie działa lokalnie na wzorcu Twojego głosu; dopiero gdy dźwięk brzmi jak „Hej Limbo”, krótki fragment idzie do rozpoznania mowy (Groq/Gemini), żeby potwierdzić. Czułość: niska/średnia/wysoka. Jeśli LimboAI budzi się sam — zmniejsz czułość albo naucz ponownie; jeśli nie reaguje — zwiększ. Wzorzec: `%APPDATA%\LiMBOCHAT\wake-hej-limbo.json`.

**Kolory:** ⚙ → *Kolor motywu* (8 gotowych + własny), albo głosem: „zmień kolor panelu na zielony”.

**Co potrafi:** kategoria i tytuł streamu, timeout/ban/unban, moderator i VIP (dawanie/odbieranie), ostrzeżenia (warn), tryb tarczy (Shield Mode), kolor nicku na czacie, zakłady (predictions), przypomnienia („przypomnij mi za 10 minut o wodzie”), losowanie widza z czatu („wylosuj kogoś”), otwieranie kanału w nowej zakładce, kolor panelu, czytanie na głos oznaczeń @ty (opcja), tryby czatu (slow, tylko emotki, tylko followersi/suby), czyszczenie czatu, wiadomości i ogłoszenia, shoutout, raid, klip, marker, reklama, ankiety, statystyki streamu („ilu mam widzów?”), streszczenie czatu („o czym gadają?”). Pamięta rozmowę przez 90 s.

**Bezpieczeństwo:** ban, wyczyszczenie czatu, raid, reklama i nadanie moderatora zawsze wymagają potwierdzenia kolejną komendą („tak”) — wymusza to kod wtyczki, nie tylko AI. Po aktualizacji trzeba raz zalogować się ponownie (LimboAI potrzebuje dodatkowych uprawnień Twitch).

**Głos:** polski głos systemowy Windows. Jeśli LimboAI mówi z angielskim akcentem — dodaj polski głos: Ustawienia → Czas i język → Mowa → Dodaj głosy. Głos idzie na domyślne wyjście audio; jeśli nie chcesz, by widzowie go słyszeli, przechwytuj w OBS tylko grę (Przechwytywanie dźwięku aplikacji) zamiast całego dźwięku pulpitu.

## Skróty

| Akcja | Jak |
|---|---|
| Nowa zakładka | `+` na pasku zakładek, wpisz nazwę, Enter |
| Zamknięcie zakładki | `×` albo środkowy przycisk myszy |
| Podpowiedź emotki | wpisz początek nazwy i naciśnij Tab (kolejne Taby = kolejne dopasowania) |
| Ban jednym kliknięciem | przytrzymaj Shift i kliknij 🚫 |
