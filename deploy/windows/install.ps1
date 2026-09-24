# Installs the uploader for the current user: copies the scripts next to the state folder, registers a
# Task Scheduler job (every 30 minutes and at logon), and puts a "Play MTGO" launcher on the desktop that
# uploads when the client exits. Run once from PowerShell: powershell -ExecutionPolicy Bypass -File install.ps1
param([string]$Server = 'https://bur.io/doomsday/stats/')
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:LOCALAPPDATA 'mtgctl'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item (Join-Path $PSScriptRoot 'mtgctl-upload.ps1') $dir -Force
Copy-Item (Join-Path $PSScriptRoot 'play-mtgo.ps1') $dir -Force
$run = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$dir\mtgctl-upload.ps1`" -Server `"$Server`" -Quiet"
schtasks /Create /F /TN 'mtgctl upload' /SC MINUTE /MO 30 /TR $run | Out-Null
schtasks /Create /F /TN 'mtgctl upload at logon' /SC ONLOGON /TR $run | Out-Null
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'Play MTGO.lnk'))
$lnk.TargetPath = 'powershell.exe'
$lnk.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$dir\play-mtgo.ps1`" -Server `"$Server`""
$lnk.Save()
Write-Host "installed: uploads every 30 minutes and at logon; the desktop 'Play MTGO' shortcut uploads when the client exits."
Write-Host "first run now..."
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$dir\mtgctl-upload.ps1" -Server $Server
