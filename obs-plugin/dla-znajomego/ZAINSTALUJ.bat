@echo off
rem Instaluje wtyczke LiMBOCHAT do OBS. Uruchom dwuklikiem przy zamknietym OBS.
chcp 65001 > nul
cd /d "%~dp0"
title Instalacja LiMBOCHAT dla OBS

if not exist "limbochat.dll" (
  echo.
  echo  Brak pliku limbochat.dll obok tego skryptu.
  echo  Rozpakuj CALY plik ZIP do jednego folderu i uruchom ponownie.
  echo.
  pause
  exit /b 1
)

tasklist /FI "IMAGENAME eq obs64.exe" 2>nul | find /I "obs64.exe" > nul
if not errorlevel 1 (
  echo.
  echo  OBS jest uruchomiony. Zamknij OBS i uruchom ten plik ponownie.
  echo.
  pause
  exit /b 1
)

set "DEST=%ProgramData%\obs-studio\plugins\limbochat\bin\64bit"
if not exist "%DEST%" mkdir "%DEST%" 2> nul
if not exist "%ProgramData%\obs-studio\plugins\limbochat\data" mkdir "%ProgramData%\obs-studio\plugins\limbochat\data" 2> nul
copy /Y "limbochat.dll" "%DEST%\limbochat.dll" > nul 2>&1
if errorlevel 1 goto elevate

echo.
echo  ============================================================
echo   Gotowe! LiMBOCHAT zainstalowany.
echo  ============================================================
echo.
echo  Teraz w OBS (jednorazowo):
echo    1. Menu "Doki" -^> "Niestandardowe doki przegladarki..."
echo    2. Nazwa: LiMBOCHAT     Adres URL: http://127.0.0.1:47250
echo    3. "Zastosuj" - panel pojawi sie w OBS.
echo    4. W panelu kliknij "Zaloguj" i zaloguj sie SWOIM kontem Twitch.
echo    5. LimboAI: ikona zebatki -^> wklej SWOJ klucz Groq i Gemini (albo wybierz wlasny model).
echo.
echo  Kolejne wersje zainstaluja sie SAME w trakcie streama - bez restartu OBS.
echo.
pause
exit /b 0

:elevate
rem Brak uprawnien do folderu wtyczek - poprosimy o uprawnienia administratora (raz).
if "%~1"=="elevated" goto permission_error
echo  Potrzebne uprawnienia administratora - zatwierdz okno Windows...
powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -ArgumentList 'elevated' -Verb RunAs"
exit /b 0

:permission_error
echo.
echo  Nie udalo sie skopiowac wtyczki do folderu OBS:
echo  %DEST%
echo  Sprawdz, czy OBS jest zamkniety, i sprobuj ponownie.
echo.
pause
exit /b 1
