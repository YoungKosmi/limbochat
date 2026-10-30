@echo off
rem LiMBOCHAT bez OBS: silnik w tle + panel jako osobne okno (Edge / Chrome w trybie aplikacji).
rem Wszystko dziala tak samo jak w OBS: czat, 7TV, moderacja, LimboAI, alerty, overlay, autoaktualizacje.
cd /d "%~dp0"
title LiMBOCHAT
set "URL=http://127.0.0.1:47250"

if not exist "limbochat-engine.exe" (
  echo.
  echo  Brak pliku limbochat-engine.exe obok tego skryptu.
  echo  Rozpakuj CALY plik ZIP do jednego folderu i uruchom ponownie.
  echo.
  pause
  exit /b 1
)

rem Klucze LimboAI od znajomego (jesli sa w paczce) - silnik przeniesie je do sejfu Windows.
if exist "klucze-limboai.json" (
  if not exist "%LOCALAPPDATA%\LiMBOCHAT\engine" mkdir "%LOCALAPPDATA%\LiMBOCHAT\engine" 2> nul
  copy /Y "klucze-limboai.json" "%LOCALAPPDATA%\LiMBOCHAT\engine\import-keys.json" > nul && del /Q "klucze-limboai.json"
)

rem Silnik trafia do %LOCALAPPDATA%\LiMBOCHAT\engine (tam tez pobieraja sie aktualizacje).
set "ENGINE="
for /f "usebackq delims=" %%p in (`limbochat-engine.exe --install`) do set "ENGINE=%%p"
if not defined ENGINE (
  echo  Nie udalo sie przygotowac silnika LiMBOCHAT.
  pause
  exit /b 1
)

rem Start w tle (bez okna). Gdy silnik juz dziala - np. z OBS - po prostu otworzymy panel.
powershell -NoProfile -Command "Start-Process -FilePath '%ENGINE%' -ArgumentList '--standalone' -WindowStyle Hidden"
powershell -NoProfile -Command "for($i=0;$i -lt 60;$i++){try{$null=Invoke-WebRequest -UseBasicParsing -TimeoutSec 1 '%URL%/control/health';exit 0}catch{Start-Sleep -Milliseconds 250}};exit 1" > nul 2>&1

rem Okno aplikacji (bez paska adresu): Edge, potem Chrome, w ostatecznosci zwykla przegladarka.
set "PROFILE=%LOCALAPPDATA%\LiMBOCHAT\okno"
set "BROWSER="
for %%b in ("%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe" "%ProgramFiles%\Microsoft\Edge\Application\msedge.exe" "%ProgramFiles%\Google\Chrome\Application\chrome.exe" "%ProgramFiles(x86)%\Google\Chrome\Application\chrome.exe" "%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe") do (
  if not defined BROWSER if exist "%%~b" set "BROWSER=%%~b"
)
if defined BROWSER (
  start "" "%BROWSER%" --app=%URL% "--window-size=460,900" --user-data-dir="%PROFILE%" --no-first-run --no-default-browser-check
) else (
  start "" %URL%
)
exit /b 0
