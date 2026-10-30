@echo off
rem LiMBOCHAT na komputer (bez OBS): ZIP z silnikiem i LiMBOCHAT.bat - panel jako okno.
cd /d "%~dp0"
title LiMBOCHAT - paczka na komputer
powershell -NoProfile -ExecutionPolicy Bypass -File "skrypty\pakiet-przegladarka.ps1"
echo.
pause
