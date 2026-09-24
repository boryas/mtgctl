# mtgctl uploader for Windows

Open PowerShell (Start menu, type "PowerShell") and paste:

    irm https://bur.io/doomsday/stats/windows/install.ps1 | iex

Or download [install-mtgctl.cmd](install-mtgctl.cmd), double-click it and click *Run* when
Windows asks; it runs the same line.

Either way it downloads the uploader, puts a **Play MTGO** shortcut on your desktop that starts
the client and uploads when you close it, registers an upload at logon as a fallback, and runs the
first upload. The first run reads every match log you have and can take a few minutes; after
that it is seconds. It works out who you are from your match logs; nothing else is needed.

Read before you run: [install.ps1](install.ps1) downloads the other two and registers the tasks;
[mtgctl-upload.ps1](mtgctl-upload.ps1) finds the logs, works out your name, and uploads what the
server lacks; [play-mtgo.ps1](play-mtgo.ps1) starts the client and uploads when it exits;
[install-mtgctl.cmd](install-mtgctl.cmd) is the one PowerShell line in a double-clickable file.
Nothing leaves your machine except your MTGO logs, and only to the stats server.

Files live in `%LOCALAPPDATA%\mtgctl`; progress is in `upload.log` there.

To remove: `schtasks /Delete /TN "mtgctl upload at logon" /F`,
then delete `%LOCALAPPDATA%\mtgctl` and the desktop shortcut.
