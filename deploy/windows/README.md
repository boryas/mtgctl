# mtgctl uploader for Windows

Open PowerShell (Start menu, type "PowerShell") and paste:

    irm https://bur.io/doomsday/stats/windows/install.ps1 | iex

That downloads the uploader, registers it to run every 30 minutes and at logon, puts a
**Play MTGO** shortcut on your desktop that uploads when you close the client, and runs the
first upload. The first run reads every match log you have and can take a few minutes; after
that it is seconds. It works out who you are from your match logs; nothing else is needed.

Files live in `%LOCALAPPDATA%\mtgctl`; progress is in `upload.log` there.

To remove: `schtasks /Delete /TN "mtgctl upload" /F`, `schtasks /Delete /TN "mtgctl upload at logon" /F`,
then delete `%LOCALAPPDATA%\mtgctl` and the desktop shortcut.
