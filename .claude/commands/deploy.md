---
description: Build mtgctl-web and deploy it to bur.io with deploy/deploy-web.sh, then check the live site
allowed-tools: Bash(deploy/deploy-web.sh)
---
The user invoked /deploy. That authorizes one remote action: running `deploy/deploy-web.sh` from the repo root.

1. Note `git log -1 --oneline` and whether the working tree has uncommitted changes (the script builds from the working tree).
2. Run `deploy/deploy-web.sh` exactly as written: no arguments, no `bash` or `./` prefix, nothing chained. Do not run ssh, scp or rsync to bur.io yourself, before or after, not even to debug a failure.
3. If ssh cannot authenticate, ask the user to run `! ssh-add`, then run the script once more. On any other failure, report the output and stop.
4. Check the live site with curl only: `https://bur.io/mtg/stats/` and `https://bur.io/mtg/stats/api/schema` answer 200, and a POST of `{"jsonrpc":"2.0","id":1,"method":"tools/list"}` (Content-Type: application/json) to `https://bur.io/mtg/stats/mcp` lists the tools.
5. Report what was deployed (commit, uncommitted changes included or not) and the checks.

nginx changes are never part of this: they need the user's sudo on bur.io.
