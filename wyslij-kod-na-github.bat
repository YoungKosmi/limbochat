@echo off
rem Wysyla kod zrodlowy na GitHuba (repozytorium z aktualizacji) i ustawia opis.
rem Cala logika jest w skrypty\wyslij-kod.ps1.
cd /d "%~dp0"
title LiMBOCHAT - wyslij kod na GitHuba
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\wyslij-kod.ps1"
echo.
pause
