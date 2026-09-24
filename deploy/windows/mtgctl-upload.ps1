# Uploads new MTGO logs to the mtgctl stats site and refreshes the results. Same steps as the "add my
# logs" page: identify the player from the match logs, send what the server does not have (match logs
# by name+size+mtime, session logs by hash, as a small extract), then sync.
# Windows PowerShell 5.1 or later; nothing to install.
param(
    [string]$Server = 'https://bur.io/doomsday/stats/',
    # MTGO client folder; the match logs live under Apps\2.0\...\Logs
    [string]$LogsRoot = (Join-Path $env:LOCALAPPDATA 'Apps\2.0'),
    # extra folders of archived session logs (mtgo-YYYYMMDD-HHMMSS.log)
    [string[]]$Archive = @(),
    [string]$StateDir = (Join-Path $env:LOCALAPPDATA 'mtgctl'),
    [switch]$Quiet
)
$ErrorActionPreference = 'Stop'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null
$logFile = Join-Path $StateDir 'upload.log'
function Log($msg) { $line = "{0:yyyy-MM-dd HH:mm:ss} {1}" -f (Get-Date), $msg; Add-Content -Path $logFile -Value $line; if (-not $Quiet) { Write-Host $line } }
$stateFile = Join-Path $StateDir 'state.json'
$state = if (Test-Path $stateFile) { Get-Content $stateFile -Raw | ConvertFrom-Json } else { [pscustomobject]@{ player = $null; last_change = $null } }
if (-not ($state.PSObject.Properties.Name -contains 'last_change')) { $state | Add-Member -NotePropertyName last_change -NotePropertyValue $null }
$Server = $Server.TrimEnd('/') + '/'

# ---- multipart/form-data by hand: works on PowerShell 5.1 (no -Form) and needs no curl
function Send-Multipart([string]$url, [string]$player, [object[]]$parts) {
    $boundary = '----mtgctl' + [guid]::NewGuid().ToString('N')
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.StreamWriter($ms, (New-Object System.Text.UTF8Encoding($false)))
    foreach ($p in $parts) {
        $w.Write("--$boundary`r`n")
        if ($p.ContainsKey('bytes')) {
            $w.Write("Content-Disposition: form-data; name=`"$($p.name)`"; filename=`"$($p.filename)`"`r`nContent-Type: application/octet-stream`r`n`r`n")
            $w.Flush(); $ms.Write($p.bytes, 0, $p.bytes.Length); $w.Write("`r`n")
        } else {
            $w.Write("Content-Disposition: form-data; name=`"$($p.name)`"`r`n`r`n$($p.value)`r`n")
        }
    }
    $w.Write("--$boundary--`r`n"); $w.Flush()
    $headers = @{}
    if ($player) { $headers['X-Player'] = $player }
    $r = Invoke-WebRequest -Uri $url -Method Post -Headers $headers -ContentType "multipart/form-data; boundary=$boundary" -Body $ms.ToArray() -UseBasicParsing
    $r.Content | ConvertFrom-Json
}
function Api([string]$path, [string]$method = 'Get') {
    $headers = @{}
    if ($state.player) { $headers['X-Player'] = $state.player }
    (Invoke-WebRequest -Uri ($Server + $path) -Method $method -Headers $headers -UseBasicParsing).Content | ConvertFrom-Json
}
# Same reduction the site applies to a raw session log: the lines that name decks, games and events, plus one line per hour.
function Session-Extract([string]$path) {
    $out = New-Object System.Collections.Generic.List[string]
    $lastHour = $null
    foreach ($line in [System.IO.File]::ReadLines($path)) {
        $hour = if ($line.Length -ge 9 -and $line[2] -eq ':' -and $line[5] -eq ':' -and $line[8] -eq ' ') { $line.Substring(0, 2) } else { $null }
        if ($line.Contains('Deck Used in Game ID:') -or $line.Contains('Game Play Status Update for Game ID:') -or $line.Contains('CreateAndFlipToDuelScene') `
            -or $line.StartsWith('Description=') -or $line.StartsWith('EventId=') -or $line.Contains('Deck Used to Join Event ID:') `
            -or ($hour -and $hour -ne $lastHour)) { $out.Add($line) }
        if ($hour) { $lastHour = $hour }
    }
    $text = [string]::Join("`n", $out) + "`n"
    $raw = [System.Text.Encoding]::UTF8.GetBytes($text)
    $gz = New-Object System.IO.MemoryStream
    $zs = New-Object System.IO.Compression.GZipStream($gz, [System.IO.Compression.CompressionMode]::Compress)
    $zs.Write($raw, 0, $raw.Length); $zs.Close()
    $gz.ToArray()
}
$epoch = [datetime]'1970-01-01T00:00:00Z'
function MtimeMs($file) { [long](($file.LastWriteTimeUtc - $epoch).TotalMilliseconds) }

# ---- find the logs
$roots = @($LogsRoot) + $Archive | Where-Object { Test-Path $_ }
if (-not $roots) { Log "no MTGO folder at $LogsRoot"; exit 1 }
$games = @(); $sessions = @()
foreach ($root in $roots) {
    $games += Get-ChildItem -Path $root -Recurse -File -Filter 'Match_GameLog_*.dat' -ErrorAction SilentlyContinue
    $sessions += Get-ChildItem -Path $root -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq 'mtgo.log' -or $_.Name -match '^mtgo-\d{8}-\d{6}\.log$' }
}
if (-not $games -and -not $sessions) { Log "no MTGO logs under $($roots -join ', ')"; exit 1 }
# nothing on disk changed since the last run: no network at all (the timed task runs overnight too)
$latest = (($games + $sessions) | Measure-Object -Property LastWriteTimeUtc -Maximum).Maximum.ToString('o')
if ($state.player -and $state.last_change -and $latest -le $state.last_change) { if (-not $Quiet) { Log 'no log changed since the last run' }; exit 0 }

# ---- who: the newest match logs name the player
if (-not $state.player) {
    if (-not $games) { Log 'only session logs found and no player known yet; run once with the MTGO client folder'; exit 1 }
    $parts = @()
    foreach ($f in ($games | Sort-Object LastWriteTimeUtc | Select-Object -Last 12)) { $parts += @{ name = 'file'; filename = $f.Name; bytes = [System.IO.File]::ReadAllBytes($f.FullName) } }
    $who = Send-Multipart ($Server + 'api/detect') $null $parts
    $state.player = $who.name
    $state | ConvertTo-Json | Set-Content $stateFile
    Log "you are $($state.player)"
}

# ---- what the server already has
$before = Api 'api/summary'
$manifest = Api 'api/manifest'
$have = @{}; $haveHash = @{}
foreach ($m in $manifest) { if ($m.kind -eq 'game') { $have[$m.name] = $m } elseif ($m.kind -eq 'session') { $haveHash[$m.sha256] = $true } }
$newest = @{}
foreach ($f in $games) { if (-not $newest.ContainsKey($f.Name) -or $f.LastWriteTimeUtc -gt $newest[$f.Name].LastWriteTimeUtc) { $newest[$f.Name] = $f } }
$todo = New-Object System.Collections.Generic.List[object]
foreach ($f in $newest.Values) {
    $h = $have[$f.Name]
    if (-not $h -or $f.Length -gt $h.size -or (MtimeMs $f) -gt ($h.mtime + 1000)) { $todo.Add(@{ kind = 'game'; file = $f }) }
}
foreach ($f in $sessions) {
    $sha = (Get-FileHash -Path $f.FullName -Algorithm SHA256).Hash.ToLower()
    if ($haveHash.ContainsKey($sha)) { continue }
    $haveHash[$sha] = $true
    $todo.Add(@{ kind = 'session-extract'; file = $f; sha256 = $sha; bytes = (Session-Extract $f.FullName) })
}
Log "$($games.Count) match logs and $($sessions.Count) session logs on disk; $($todo.Count) new to upload"

# ---- upload in batches of at most 48 MB; field order matters (kind, name, mtime, sha256, then file)
$maxBatch = 48MB
$i = 0
while ($i -lt $todo.Count) {
    $parts = @(); $bytes = 0; $j = $i
    while ($j -lt $todo.Count) {
        $t = $todo[$j]
        $data = if ($t.ContainsKey('bytes')) { $t.bytes } else { [System.IO.File]::ReadAllBytes($t.file.FullName) }
        if ($j -gt $i -and $bytes + $data.Length -gt $maxBatch) { break }
        $parts += @{ name = 'kind'; value = $t.kind }
        $parts += @{ name = 'name'; value = $t.file.Name }
        $parts += @{ name = 'mtime'; value = (MtimeMs $t.file) }
        if ($t.ContainsKey('sha256')) { $parts += @{ name = 'sha256'; value = $t.sha256 } }
        $parts += @{ name = 'file'; filename = $t.file.Name; bytes = $data }
        $bytes += $data.Length; $j++
    }
    Send-Multipart ($Server + 'api/upload') $state.player $parts | Out-Null
    Log ("uploaded {0}-{1} of {2} ({3:N1} MB)" -f ($i + 1), $j, $todo.Count, ($bytes / 1MB))
    $i = $j
}

$state.last_change = $latest
$state | ConvertTo-Json | Set-Content $stateFile
if ($todo.Count -eq 0) { Log "nothing new; latest match on file $($before.latest_match)"; exit 0 }

# ---- refresh the results and say what changed, in match terms
$rep = Api 'api/sync' 'Post'
$after = Api 'api/summary'
$new = $after.matches - $before.matches
if ($new -gt 0) { Log "$new new match(es); $($after.matches) on file, latest $($after.latest_match)" } else { Log "no new matches; latest $($after.latest_match)" }
if ($rep.in_progress) { Log "$($rep.in_progress) match(es) not finished yet" }
if ($after.latest_list) { Log "latest list: $($after.latest_list)" }
