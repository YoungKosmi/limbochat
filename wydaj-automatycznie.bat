@echo off
rem Wydanie aktualizacji bez pytan (nastepny numer wersji) - log: wydania\ostatnie-wydanie.log
cd /d "%~dp0"
title LiMBOCHAT - wydaj-automatycznie
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\wydaj-aktualizacje.ps1" -Auto
echo.
echo Okno zamknie sie samo za 60 sekund.
timeout /t 60 >nul
