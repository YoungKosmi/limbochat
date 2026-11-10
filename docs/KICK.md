# Kick w LiMBOCHAT

Obsługa Kicka jest w silniku OBS i pakiecie **LiMBOCHAT-na-komputer.zip** (panel przeglądarkowy). Starsza aplikacja egui z `src/main.rs` pozostaje klientem Twitcha.

Przy przycisku **+** wybierz Twitch albo Kick i wpisz nazwę kanału. Te same nazwy na obu platformach otwierają osobne karty. **Ustawienia → Połączone konta** pozwalają połączyć oba konta jednocześnie. Aktywna karta określa platformę dla czatu i poleceń LimboAI. Tytuł i kategoria zmieniają własny kanał zalogowanego konta tej platformy; moderacja dotyczy aktywnej karty.

## Pierwsze połączenie Kicka

1. W ustawieniach developerskich Kicka utwórz aplikację. Ustaw dokładnie ten Redirect URI: `http://localhost:47250/kick/callback`.
2. W LiMBOCHAT otwórz **Ustawienia → Konfiguracja aplikacji Kick**, wpisz Client ID i Client Secret, a następnie zapisz. Sekret nie wraca do panelu; pole służy tylko do jego zastąpienia. Client ID jest jawnym identyfikatorem aplikacji, nie tokenem.
3. Kliknij **Połącz** przy Kicku i zatwierdź zgodę w przeglądarce. Po przekierowaniu wróć do LiMBOCHAT. Logowanie Twitcha pozostaje niezależne.
4. Do odbioru czatu skonfiguruj publiczny adres HTTPS webhooka według sekcji poniżej. Zaznacz odbieranie webhooków, zapisz, otwórz kartę Kicka i kliknij **Połącz ponownie kanały Kick**.

## Webhook czatu i alertów

Oficjalne API Kicka dostarcza zdarzenia przez webhooki. Samo logowanie nie uruchamia odbioru czatu. Potrzebny jest tunel HTTPS lub własny reverse proxy, kierujący **wyłącznie** do `http://127.0.0.1:47251`. Nie kieruj go na panel sterowania `47250`.

W aplikacji developerskiej Kicka włącz webhooki i ustaw URL `https://TWÓJ-PUBLICZNY-ADRES/kick/events`. Tunnel/reverse proxy musi zachować ścieżkę, surowe bajty POST i nagłówki `Kick-Event-*`. Lokalny odbiornik ma tylko endpoint `/kick/events`; panel, OAuth callback i komendy sterujące są na osobnym porcie.

LiMBOCHAT pobiera aktualny klucz publiczny Kicka, weryfikuje podpis RSA/SHA-256, czas zdarzenia i subskrypcję powiązaną z kanałem. Powtórzone dostarczenia nie dublują wiadomości. Brak publicznego adresu, wyłączony tunel lub wyłączone webhooki oznaczają brak nowych wiadomości. W ustawieniach widać liczbę przyjętych subskrypcji i czas ostatniego zdarzenia. Przyjęta subskrypcja nie potwierdza jeszcze poprawnego działania tunelu.

Subskrypcje są zakładane tokenem aplikacji z jawnym ID wybranego nadawcy. Jest to istotne: Kick przy tokenie użytkownika ignoruje podane ID i wybiera jego własny kanał. Po zamknięciu karty jej zdarzenia są ignorowane; subskrypcje aplikacji pozostają do ponownego połączenia. Nie usuwamy subskrypcji, które mogą być używane przez inną instancję tej samej aplikacji.

Każda instalacja powinna mieć własną aplikację Kick i własny adres odbioru. Nie dołączaj Client Secret ani tokenów do paczek publicznych. Zmiana adresu w Kicku wymaga ponownego uruchomienia/ustawienia tunelu; program nie tworzy publicznego tunelu automatycznie.

## Dostępne funkcje

| Funkcja | Kick |
| --- | --- |
| Wiele kanałów, historia czatu, wzmianki, linki i podglądy | Tak, po skonfigurowaniu webhooka |
| Wiadomości jako zalogowane konto | Tak, zgoda `chat:write` |
| Natywne emotki | Wyświetlanie; picker i wysyłanie emotek widzianych w bieżącej historii |
| 7TV | Globalne i kanałowe przez połączenie konta Kick z 7TV |
| Usuwanie, ban, unban, timeout | Tak, API sprawdza uprawnienia do kanału |
| Status moderatora | Właściciel kanału; moderator rozpoznany po jego podpisanej wiadomości z odznaką |
| Timeout | 1–10080 minut; krótsze polecenia głosowe zaokrąglane w górę do minuty |
| Alerty | Follow, nowe/odnowione suby, gifty, KICKs, realizacje nagród o statusie `pending` |
| LimboAI | Rozmowa, odczyt czatu, wiadomości, moderacja, tytuł i kategoria |
| Studio | Quiz, boss, momenty, powtórki OBS, analiza obrazu, profile, karta widza i notatki |
| Pamięć o widzach | Oddzielne klucze tożsamości Kick/Twitch; notatki Studia dodatkowo osobne dla kanału |
| Podsumowanie streama | Dla kanału wybranej platformy; OBS lub okresowe sprawdzanie API wykrywa transmisję |
| Ankiety i predykcje Kicka | Brak endpointów w publicznym API; obsługa w panelu Kicka |
| Raidy, klipy, role, tryby czatu, tarcza, kolor nicku, ogłoszenia, ostrzeżenia i reklamy | Niezaimplementowane dla Kicka; narzędzie zwraca czytelny błąd zamiast wykonywać akcję na Twitchu |

Zdarzenia usunięcia wiadomości i odbanowania wykonane poza LiMBOCHAT nie są dostępne w użytym oficjalnym feedzie. Lokalne potwierdzone akcje UI są odnotowywane. Historia nie jest pełnym archiwum platformy. KICKs są osobnym rodzajem alertu, nie bitsami Twitcha.

## Dane i testy

Publiczna konfiguracja aplikacji jest w `obs-dock.json`. Client Secret i sesja OAuth mają osobne wpisy w systemowym sejfie. Wylogowanie Kicka nie wylogowuje Twitcha. Profile Studia nie kopiują danych logowania. Tryb demo nie zapisuje ani nie usuwa poświadczeń Kicka.

Do konfiguracji skryptowej można przekazać JSON z polami `client_id`, `client_secret` i opcjonalnym `webhooks` na standardowe wejście polecenia `limbochat-engine.exe --configure-kick`. Nie podawaj sekretu w argumentach procesu. Polecenie zapisuje konfigurację lokalnie i nie wykonuje logowania ani zmian w panelu Kicka.

Testy Rust sprawdzają podpisane webhooki, odrzucenie zmian treści i kanału, deduplikację, timeouty w minutach, endpointy moderacji, token aplikacji dla subskrypcji i ochronę OAuth state. Test `node tests/studio-e2e.mjs` sprawdza równoczesne kanały Twitch/Kick, izolację notatek, quiz i odmowę przeniesienia głosowań na Twitcha. Atrapy nie zastępują sprawdzenia logowania i dostarczania webhooków na rzeczywistym koncie.

Źródła: [Kick OAuth 2.1](https://docs.kick.com/getting-started/generating-tokens-oauth2-flow), [webhooki i podpisy](https://docs.kick.com/events/webhook-security), [typy zdarzeń](https://docs.kick.com/events/event-types), [oficjalne OpenAPI](https://api.kick.com/swagger/index.html).
