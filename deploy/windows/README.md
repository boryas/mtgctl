# mtgctl uploader for Windows

Log in with Discord on the upload page (https://bur.io/mtg/stats/collect.html) and press *Make my
install line*. Open PowerShell (Start menu, type "PowerShell") and paste the line it shows:

    $env:MTGCTL_KEY='mk_...'; irm https://bur.io/mtg/stats/windows/install.ps1 | iex

Or download the install-mtgctl.cmd the page offers next to the line, double-click it and click
*Run* when Windows asks; it runs the same line.

Either way it saves your uploader key, downloads the uploader, puts a **Play MTGO** shortcut on
your desktop that starts the client and uploads when you close it, registers an upload at logon as
a fallback, and runs the first upload. The first run reads every match log you have and can take a
few minutes; after that it is seconds. It works out your MTGO name from your match logs.

Read before you run: [install.ps1](install.ps1) saves the key, downloads the other two and
registers the tasks; [mtgctl-upload.ps1](mtgctl-upload.ps1) finds the logs, works out your name,
and uploads what the server lacks; [play-mtgo.ps1](play-mtgo.ps1) starts the client and uploads
when it exits. Nothing leaves your machine except your MTGO logs, and only to the stats server.

Files live in `%LOCALAPPDATA%\mtgctl` (the key in `key`); progress and refusals are in
`upload.log` there.

To remove: `schtasks /Delete /TN "mtgctl upload at logon" /F`, then delete
`%LOCALAPPDATA%\mtgctl` and the desktop shortcut, and revoke the key on the upload page.
