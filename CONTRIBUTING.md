# Praca nad LiMBOCHAT

Instrukcje budowania, mapa plików i zasady wydań są w [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

1. Zacznij od aktualnego `main` i utwórz gałąź dla konkretnej zmiany.
2. Do poprawki błędu dodaj test odtwarzający problem, jeśli można go sprawdzić automatycznie.
3. Uruchom testy opisane w dokumentacji i wpisz zmianę w `CHANGELOG.md` pod „Następna wersja”.
4. W pull requeście opisz wynik i ograniczenia weryfikacji. Test na danych przykładowych nie zastępuje próby na żywym kanale.

Nie dodawaj tokenów, kluczy API, haseł OBS, pamięci widzów ani prywatnych paczek do repozytorium lub zgłoszeń. Luki zgłaszaj zgodnie z [SECURITY.md](SECURITY.md).
