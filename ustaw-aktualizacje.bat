@echo off
rem Jednorazowo: repozytorium GitHub na autoaktualizacje + klucz podpisu.
rem Cala logika jest w skrypty\ustaw-aktualizacje.ps1 (PowerShell jest w kazdym Windows 10/11).
cd /d "%~dp0"
title LiMBOCHAT - ustaw-aktualizacje
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\ustaw-aktualizacje.ps1"
echo.
pause
