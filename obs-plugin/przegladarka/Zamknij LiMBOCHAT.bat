@echo off
rem Zatrzymuje LiMBOCHAT dzialajacy w tle (silnik i tak wylaczy sie sam 3 minuty po zamknieciu okna).
cd /d "%~dp0"
limbochat-engine.exe --stop
timeout /t 2 > nul
