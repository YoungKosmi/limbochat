@echo off
rem Buduje LiMBOCHAT i jednoplikowy instalator LiMBOCHAT-Setup.exe.
rem Uruchom dwuklikiem albo z wiersza polecen w folderze projektu.
chcp 65001 > nul
cd /d "%~dp0"

echo.
echo  [1/2] Budowanie LiMBOCHAT...
cargo build --release -p limbochat
if errorlevel 1 goto error

echo.
echo  [2/2] Pakowanie instalatora...
cargo build --release -p limbochat-setup
if errorlevel 1 goto error

copy /Y "target\release\LiMBOCHAT-Setup.exe" "LiMBOCHAT-Setup.exe" > nul

echo.
echo  Gotowe! Instalator: %cd%\LiMBOCHAT-Setup.exe
echo  Wyslij ten jeden plik znajomemu.
echo.
pause
exit /b 0

:error
echo.
echo  Budowanie nie powiodlo sie - sprawdz komunikat powyzej.
echo.
pause
exit /b 1
