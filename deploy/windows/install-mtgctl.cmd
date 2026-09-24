@echo off
rem Installs the mtgctl uploader: same as pasting the one line from the site into PowerShell.
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://bur.io/doomsday/stats/windows/install.ps1 | iex"
echo.
pause
