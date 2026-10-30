@echo off
rem Buduje wtyczke LiMBOCHAT i instaluje ja w OBS na tym komputerze (OBS zamkniety).
rem Cala logika jest w skrypty\zainstaluj-lokalnie.ps1 (PowerShell jest w kazdym Windows 10/11).
cd /d "%~dp0"
title LiMBOCHAT - zainstaluj-lokalnie
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\zainstaluj-lokalnie.ps1"
echo.
pause
