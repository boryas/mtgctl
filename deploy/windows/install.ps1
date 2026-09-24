# Installs the mtgctl uploader for the current user. Run from PowerShell with one line:
#   irm https://bur.io/doomsday/stats/windows/install.ps1 | iex
# Downloads the uploader and the launcher next to the state folder, puts a "Play MTGO" shortcut on
# the desktop that uploads when the client exits, registers an upload at logon (catches a session
# played without the shortcut, as long as the client has not been started again since), and runs
# the first upload. Set MTGCTL_TIMER=1 before running to also upload every 30 minutes.
$ErrorActionPreference = 'Stop'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}
$Server = if ($env:MTGCTL_SERVER) { $env:MTGCTL_SERVER } else { 'https://bur.io/doomsday/stats/' }
$dir = Join-Path $env:LOCALAPPDATA 'mtgctl'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
foreach ($f in 'mtgctl-upload.ps1', 'play-mtgo.ps1') {
    Invoke-WebRequest -Uri ($Server.TrimEnd('/') + "/windows/$f") -OutFile (Join-Path $dir $f) -UseBasicParsing
}
$run = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$dir\mtgctl-upload.ps1`" -Server `"$Server`" -Quiet"
schtasks /Create /F /TN 'mtgctl upload at logon' /SC ONLOGON /TR $run | Out-Null
if ($env:MTGCTL_TIMER) { schtasks /Create /F /TN 'mtgctl upload' /SC MINUTE /MO 30 /TR $run | Out-Null }
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'Play MTGO.lnk'))
$lnk.TargetPath = 'powershell.exe'
$lnk.Arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$dir\play-mtgo.ps1`" -Server `"$Server`""
$lnk.Save()
Write-Host "Installed. Start MTGO from the desktop 'Play MTGO' shortcut: your logs upload when you close it (and at logon, as a fallback)."
Write-Host "First upload now (this reads every match log you have; later runs take seconds)..."
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $dir 'mtgctl-upload.ps1') -Server $Server
Write-Host "Done. Your results: $Server"
