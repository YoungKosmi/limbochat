# Co nowego w LiMBOCHAT

Nowe wpisy dopisujemy na górze, w sekcji „Następna wersja”. Skrypt `wydaj-aktualizacje.bat`
sam zamienia ją na numer wersji i wysyła jako opis wydania — a po aktualizacji ta lista
pokazuje się w panelu w okienku „Co nowego”.

## Następna wersja

- ⬆️ **LimboAI pyta o aktualizacje** — gdy wyjdzie nowa wersja, LimboAI powie o tym i zapyta, czy zainstalować. Powiedz „zainstaluj”, a zrobi to od razu (bez restartu OBS), albo zapytaj „co nowego?”. Możesz też w każdej chwili powiedzieć „sprawdź, czy jest nowa wersja”. Bez odpowiedzi nic się nie dzieje — możesz zainstalować później w ⚙ → Aktualizacje. Wolisz jak dawniej? ⚙ → Aktualizacje → Automatycznie.
- 🗣️ **Naturalny głos LimboAI** — LimboAI mówi teraz głosem Gemini, który brzmi jak człowiek, a nie jak robot. Barwę głosu wybierzesz w ⚙ → LimboAI → Głos LimboAI. Gdy Gemini nie działa (limit, brak internetu), LimboAI sam wraca do głosu systemowego Windows.
- 📊 **Zużycie AI dzisiaj** — w ⚙ → LimboAI widać, ile zapytań do Gemini (mózg i głos) oraz rozpoznawania mowy poszło od rana i czy skończył się limit.

## 0.7.1 — 2026-10-30

- Wersja wydana przez pomyłkę ze starego kodu — bez nowego głosu i licznika. Poprawione w następnej wersji.

## 0.7.0 — 2026-10-30

- **LiMBO Studio** — nowy panel z przycisku ✦ przy czacie: momenty, głosowania, gry, obraz, karta widza i profile.
- **Karta moderacji widza** — ostatnie wiadomości, lokalna historia kar, prywatne notatki osobne dla kanału oraz szybkie timeouty/ban/unban.
- **Panel ankiet i predykcji** — wyniki, ostatnie głosowania, tworzenie i kończenie, wskazanie zwycięzcy predykcji, wynik na overlayu i ogłoszenie przez Limbo.
- **Znaczniki momentów** — czas streama, opis, fragment czatu i lista do montażu; działa też komenda głosowa „zapamiętaj ten moment”.
- **Reżyser powtórek OBS** — zapis potwierdzony nazwą pliku z OBS, odtwarzanie w wybranym źródle multimediów i przywrócenie jego ustawień.
- **Profile streama** — zapis i przełączanie wyglądu, alertów, overlayu i LimboAI; dodano regulację głośności głosu.
- **Limbo widzi grę** — analiza pojedynczego kadru z wybranego źródła OBS, z podglądem przed wysłaniem do wybranego modelu AI.
- **Teleturniej Limbo** — własna baza pytań, odpowiedzi z czatu, odliczanie, punkty za poprawność i szybkość, finał oraz overlay.
- **Czat kontra boss** — wspólna walka pytaniami i komendą !atak, pasek HP, ranking obrażeń i opcjonalna scena OBS po zwycięstwie.
- Automatyczna aktualizacja czeka na zakończenie gier i operacji Studia.

## 0.6.7 — 2026-10-30

- Połączenie IRC korzysta z systemowego TLS; usunięto stare zależności rustls 0.21 i webpki 0.101 zgłaszane przez GitHub.
- Pamięć widzów ponawia zapis po błędzie dysku zamiast pomijać niezapisane zmiany.
- Kilka nakładek zero-width 7TV poprawnie łączy się z emotką Twitcha także po opóźnionym załadowaniu emotek.
- Okno „Co nowego” pomija niewydane zmiany i nie powtarza bieżącej wersji jako nowej aktualizacji.
- Wydania wskazują commit właściwych źródeł; skrypt blokuje istniejące tagi i nie usuwa wydań po błędzie połączenia.
- Dodano automatyczne testy Windows na GitHubie, formularze zgłoszeń, dokumentację rozwoju i reguły plików lokalnych.

## 0.6.6 — 2026-10-26

- 📊 **Wyniki ankiet i predykcji** — zapytaj LimboAI „jakie są wyniki ankiety?” albo „kto wygrał predykcję?” także po jej zakończeniu: poda zwycięzcę, głosy i procenty. Przy kończeniu ankiety od razu mówi, co wygrało.
- 🔥 **Seria oglądania na czacie** — gdy ktoś udostępni „oglądam X streamów z rzędu” razem z wiadomością, pojawia się karta z płomieniem i jego tekstem (wcześniej wiadomość znikała). Ogłoszenia moderatorów (/announce) też są widoczne na czacie.

## 0.6.5 — 2026-10-26

- 🧠 **Pamięć LimboAI o widzach** — LimboAI pamięta, kto lubi koty, w co gra i ile już z Tobą ogląda. Powiedz „zapamiętaj, że kasia lubi koty” albo dopisz notatkę w karcie widza (kliknij nick). Może też sam uczyć się z czatu.
- 📊 **Podsumowanie streama** — po kliknięciu „Zakończ stream” w OBS pojawia się okno ze statystykami: czas, widzowie, wiadomości, najaktywniejsi, bitsy, suby, wykresy i podsumowanie słowami od LimboAI.
- 🎬 **LimboAI steruje OBS-em** — „przełącz na scenę Przerwa”, „wycisz mikrofon”, „schowaj kamerkę”, „zacznij nagrywać”, „zapisz powtórkę”. Wystarczy włączyć serwer WebSocket w OBS i wkleić hasło w ⚙ → OBS.
- ✨ **Okienko „Co nowego”** — po każdej aktualizacji zobaczysz, co się zmieniło (właśnie na nie patrzysz).
- 🖥️ **LiMBOCHAT na komputer bez OBS** — `LiMBOCHAT.bat` otwiera panel jako osobne okno, wszystko działa tak samo.

## 0.6.0

- 👑 Powitania VIP: twórca LiMBOCHAT i własna lista — głos, złota karta i efekt na overlayu.
- 🔑 Klucze LimboAI w prywatnej paczce dla znajomych — działa bez zakładania kont.
- 🎉 Animowane alerty: bitsy, suby, gifty, gift-bomby, raidy, follow, nagrody za punkty.
- ✍️ Rymowanki LimboAI do nicku osoby z alertu.
- 🖼️ Obrazki z kappa.lol prosto na czacie.

## 0.5.0

- 🔄 Autoaktualizacje bez restartu OBS — podpisane, z przekazaniem czatu.
- 🤖 Własny model AI (OpenAI, OpenRouter, Groq, Ollama, LM Studio), przycisk do mówienia (push-to-talk) i overlay na stream.
- 🎙️ „Hej Limbo” — aktywacja głosem, kolory motywu, moderator/VIP/ostrzeżenia głosem.
