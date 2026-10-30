@echo off
rem Paczka ZIP dla znajomych (wtyczka z wbudowanym silnikiem + instalator).
rem Cala logika jest w skrypty\pakiet-dla-znajomego.ps1 (PowerShell jest w kazdym Windows 10/11).
cd /d "%~dp0"
title LiMBOCHAT - pakiet-dla-znajomego
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\pakiet-dla-znajomego.ps1"
echo.
pause
