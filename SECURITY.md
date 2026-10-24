# Bezpieczeństwo

LiMBOCHAT działa na komputerze streamera i ma dostęp do jego konta Twitch, dlatego bezpieczeństwo jest wbudowane od początku:

- **Tokeny i klucze API** są tylko w sejfie Windows (Menedżer poświadczeń) — nigdy w plikach ustawień, logach ani w panelu.
- **Panel i overlay** działają tylko na `127.0.0.1`. Serwer sprawdza nagłówki `Host` i `Origin`, więc żadna strona otwarta w przeglądarce nie wyśle komendy (np. bana) w Twoim imieniu. Overlay ma osobne połączenie tylko do odczytu.
- **Aktualizacje** są podpisane kluczem Ed25519 (podpis obejmuje wersję, nazwę pliku i SHA-256). Silnik bez poprawnego podpisu nie zainstaluje pliku, nawet gdyby ktoś przejął konto GitHub. Klucz prywatny nigdy nie trafia do repozytorium.
- **LimboAI** zawsze pyta o potwierdzenie przed banem, czyszczeniem czatu, raidem, reklamą i nadaniem moderatora — wymusza to kod, nie tylko model AI.
- **Obrazki z linków** (kappa.lol) pobiera silnik: tylko https, tylko obrazy/wideo, z limitem rozmiaru — adres IP streamera nie trafia do serwisu.

## Zgłaszanie problemów

Znalazłeś lukę? **Nie zakładaj publicznego zgłoszenia.** Użyj [prywatnego zgłoszenia podatności](../../security/advisories/new) na GitHubie albo napisz do autora. Postaramy się odpowiedzieć szybko.
