# LiMBO Studio

W panelu czatu kliknij **✦ Studio**. Możesz też otworzyć `http://127.0.0.1:47250/studio` w przeglądarce. Studio korzysta z tego samego silnika, logowania Twitcha i połączenia OBS co czat. Nie wymaga instalowania dodatkowej wtyczki.

## Momenty i reżyser powtórek

„Zapisz znacznik” zachowuje czas streama, opis i ostatnie 20 wiadomości z aktywnego kanału. Po zakończeniu transmisji skopiuj listę do montażu albo rozwiń fragment czatu. LimboAI może zaproponować krótki tytuł na podstawie opisu, jeśli asystent jest włączony. Głosem: „Limbo, zapamiętaj ten moment” lub „zapisz powtórkę”.

„Zapisz też powtórkę OBS” wymaga włączonego **bufora powtórek** w OBS. Powtórka zostaje oznaczona jako zapisana dopiero po zdarzeniu `ReplayBufferSaved` z nazwą pliku, nie po samym przyjęciu polecenia. Błąd OBS nie usuwa znacznika.

Do pokazywania powtórki dodaj w OBS na scenie gry źródło typu **Źródło multimediów** (np. „Powtórka”). W Studio wpisz dokładną nazwę źródła i zapisz. „Pokaż powtórkę” podstawi nagranie i pokaże źródło na bieżącej scenie. Po zakończeniu lub „Zakończ powtórkę” przywróci poprzedni plik i widoczność. Źródło VLC i przechwytywanie gry nie są źródłami odtwarzacza. Powtórka jest zatrzymywana najpóźniej po 3 minutach. Przy rozłączeniu OBS może być potrzebne ręczne przywrócenie źródła.

Usuwanie znacznika nie usuwa pliku wideo. Studio zachowuje do 200 momentów. Jeżeli źródło zmienisz ręcznie podczas odtwarzania, Studio nie zastąpi ręcznie wybranego pliku poprzednim.

## Ankiety i predykcje

Zaloguj konto nadawcy głosowania. „Odśwież wyniki” pobiera do 20 ostatnich ankiet i predykcji zwróconych przez Twitcha; lista jest zapisywana lokalnie. Możesz włączyć odświeżanie co 15 sekund, gdy zakładka jest otwarta. Konto musi mieć uprawnienia do danej funkcji na Twitchu. Przy braku uprawnień zaloguj się ponownie lub sprawdź dostępność ankiet/predykcji dla konta.

Utwórz głosowanie formularzem, zakończ ankietę, zablokuj lub anuluj predykcję albo świadomie wybierz zwycięską odpowiedź. Wyniki można pokazać na overlayu oraz ogłosić głosem Limbo. Zmiana rozstrzygnięcia nie jest zgadywana przez AI — zwycięzcę predykcji wybiera prowadzący.

Źródło przeglądarki OBS: `http://127.0.0.1:47250/arena?view=ballot`, rozmiar 1280 × 720. „Ukryj wynik” usuwa kartę z overlayu. Historia Twitcha nie jest pełnym archiwum: przechowujemy dane zwrócone przez API i lokalny zapis ostatniego odświeżenia.

## Teleturniej

W „Baza pytań” jest zestaw startowy. Dodaj własne pytania, 2–6 odpowiedzi i wskaż poprawną. Kliknij „Zapisz zestaw”. Zmiana bazy nie zmienia już trwającej gry.

Wybierz otwarty kanał, czas pytania 10–120 sekund i opcję głosu. Widzowie odpowiadają literą A–F lub `!odp A`. Liczy się pierwsza poprawnie sformatowana odpowiedź osoby w rundzie: błędnej nie można poprawić. Trafienie daje 100 punktów plus liczbę pełnych sekund pozostałych do końca. Odpowiedzi z innych kanałów, sprzed startu lub po czasie nie są liczone.

Po czasie wynik pojawia się na 6 sekund, po czym Limbo przechodzi do kolejnego pytania i finału. Prowadzący może wcześniej odkryć odpowiedź albo przejść dalej. Pytania i wyniki są czytane przy włączonym LimboAI i jego głosie; przy zajętym asystencie gra nadal działa wizualnie. Dla dłuższych pytań ustaw dłuższy czas rundy.

Overlay: `http://127.0.0.1:47250/arena?view=quiz`. Poprawna odpowiedź nie jest wysyłana do overlayu przed odkryciem wyniku. Ranking jest sesyjny; po zakończeniu „Zakończ i ukryj” zamyka grę.

## Czat kontra boss

Wybierz otwarty kanał, nazwę bossa, 20–1000 HP i czas walki 30–600 sekund. Widz atakuje przez `!atak A`. Poprawna odpowiedź zadaje 10 obrażeń i zmienia wyzwanie na następne pytanie z bazy. Błędny atak blokuje osobie ponowną próbę do zmiany wyzwania. Zwykłe litery na czacie nie są atakami. Po upływie czasu wygrywa boss; przy 0 HP wygrywa czat.

Opcjonalnie wskaż istniejącą scenę OBS, która ma zostać wybrana po zwycięstwie. Bez wyboru sceny gra nie zmienia OBS. Wynik pozostaje widoczny do „Zakończ i ukryj”. Overlay: `http://127.0.0.1:47250/arena?view=boss`.

Automatyczne instalowanie aktualizacji czeka, gdy quiz lub boss są otwarte albo trwa operacja Studia. Pozwala to uniknąć restartu silnika w środku gry. Gry nie przenoszą się przez ręczne zamknięcie silnika.

## Limbo widzi grę

1. Połącz OBS w dotychczasowych ustawieniach.
2. Pobierz listę źródeł i wybierz dokładnie to źródło, które chcesz pokazać modelowi (np. przechwytywanie gry). Zaznacz zgodę na analizę i zapisz.
3. „Pobierz podgląd kadru” pobiera pojedynczy obraz wyłącznie z wybranego źródła. Nie wysyła go do AI.
4. Obejrzyj kadr, wpisz pytanie i kliknij „Analizuj ten kadr”. Dopiero ten przycisk wysyła obraz do wybranego w LimboAI dostawcy. Odpowiedź pojawia się również na overlayu Limbo; głos można włączyć osobno.

Nie ma ciągłego oglądania ekranu. Kadr jest tylko w pamięci silnika, wygasa po 2 minutach i nie jest zapisywany w pliku danych Studia. Zmiana źródła, profilu lub wyłączenie analizy unieważnia poprzedni kadr. Wyłączenie przełącznika działa od razu. Wyłączenie po wysłaniu nie cofa już wysłanego żądania, ale jego spóźniona odpowiedź nie jest pokazywana.

Gemini otrzymuje obraz jako dane inline. Własny dostawca korzysta z formatu obrazu w Chat Completions; model musi obsługiwać obrazy. Przy błędzie nie zmieniamy dostawcy ani modelu automatycznie. Analiza nie ma narzędzi: obraz nie może wywoływać moderacji, poleceń OBS ani kodu. Odpowiedź opisuje pojedynczy kadr, więc ruch i wcześniejsze zdarzenia mogą zostać błędnie ocenione.

## Karta widza

W menu pod nickiem kliknij „Historia moderacji i wiadomości” albo wpisz nick w Studio. Karta pokazuje do 30 ostatnich zarejestrowanych wiadomości widza na tym kanale, zdarzenia banów/timeoutów/usuwania i potwierdzone odbanowania wykonane przyciskami klienta oraz prywatne notatki moderatorskie. Szybkie timeouty, ban i unban wymagają uprawnień moderatora oraz ID widza z bieżącej historii czatu.

To lokalna historia od wprowadzenia funkcji, a nie pełny dziennik Twitcha. Zdarzenia wykonane poza klientem są widoczne tylko wtedy, gdy dotarły do niego przez IRC. Notatki są osobne dla każdego kanału i nie trafiają do AI ani overlayu. Historia ma limit 1000 widzów i 2000 zdarzeń moderacji.

## Profile streama

Zapisz bieżące ustawienia pod nazwą „Granie”, „Just Chatting”, „Reakcje” itp. Profil zawiera wygląd, alerty, overlay, ustawienia LimboAI (w tym głośność głosu) oraz ustawienia analizy obrazu. Po zastosowaniu zmienia je w całym panelu. Tokeny i klucze nie są kopiowane; logowanie Twitcha, kanały i połączenie OBS pozostają niezależne. Można zapisać do 12 profili.

## Dane i sprawdzanie

Dane lokalne są w `%APPDATA%\LiMBOCHAT\studio.json`, z zapisem przez plik tymczasowy i ponawianiem przy błędzie. Arena otrzymuje tylko publiczny stan gier i wybranego głosowania. Nie otrzymuje historii moderacji, notatek, bazy poprawnych odpowiedzi, profili, zapisanych fragmentów czatu ani podglądu obrazu.

Implementacja korzysta z [protokołu OBS WebSocket](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md), [dokumentacji obrazów Gemini](https://ai.google.dev/gemini-api/docs/image-understanding) i [oficjalnej dokumentacji obrazów OpenAI](https://developers.openai.com/api/docs/guides/images-vision?api-mode=chat).
