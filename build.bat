@echo off
rem Double-click to build SoundShelf.exe into this folder and add it to the Start menu.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
if errorlevel 1 pause
