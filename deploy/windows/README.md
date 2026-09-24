# mtgctl uploader for Windows

Sends your MTGO match and session logs to the stats site automatically, the same way the
"add my logs" page does. Nothing to install: Windows PowerShell does the work.

1. Download the three `.ps1` files into one folder.
2. Right-click `install.ps1` → *Run with PowerShell* (or from a PowerShell window:
   `powershell -ExecutionPolicy Bypass -File install.ps1`).
3. That registers an upload every 30 minutes and at logon, puts a **Play MTGO** shortcut on
   your desktop that uploads when you close the client, and runs the first upload right away.

The first upload works out who you are from your match logs and remembers it in
`%LOCALAPPDATA%\mtgctl\state.json`. Progress goes to `%LOCALAPPDATA%\mtgctl\upload.log`.

To remove: `schtasks /Delete /TN "mtgctl upload" /F` and `schtasks /Delete /TN "mtgctl upload at logon" /F`,
then delete `%LOCALAPPDATA%\mtgctl` and the desktop shortcut.
