@echo off
rem Nowa wersja: budowanie, podpis i wydanie na GitHubie - znajomi dostana ja sami.
rem Cala logika jest w skrypty\wydaj-aktualizacje.ps1 (PowerShell jest w kazdym Windows 10/11).
cd /d "%~dp0"
title LiMBOCHAT - wydaj-aktualizacje
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\wydaj-aktualizacje.ps1"
echo.
pause
