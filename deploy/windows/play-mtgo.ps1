# Starts the MTGO client, waits for it to exit, then uploads: the session log is complete at that point,
# and the client overwrites it on the next launch.
param([string]$Server = 'https://bur.io/doomsday/stats/')
$ErrorActionPreference = 'Continue'
$shortcut = Get-ChildItem -Path (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs') -Recurse -Filter 'Magic The Gathering Online*.appref-ms' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($shortcut) { Start-Process $shortcut.FullName } else { Start-Process 'MTGO.exe' }
Start-Sleep -Seconds 20
while (Get-Process -Name 'MTGO' -ErrorAction SilentlyContinue) { Start-Sleep -Seconds 15 }
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'mtgctl-upload.ps1') -Server $Server -Quiet
