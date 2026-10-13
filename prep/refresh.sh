#!/bin/sh
# Refresh prep/meta.json from the last DAYS (default 28) of MTGO Legacy Challenges.
# Publishing the page afterwards is a separate step (the Artifact republish of prep/index.html + meta.json).
set -e
DAYS=${1:-28}
here=$(cd "$(dirname "$0")" && pwd)
since=$(date -d "-$DAYS days" +%F)
python3 "$here/../challenge_cache/download_challenges.py" "$since" | tail -1
python3 "$here/../challenge_cache/fetch_decklists.py" "$since"
python3 "$here/build_meta.py" "$DAYS"
# Opponent early-turn ranges from the stats site's pooled DB (copied with its WAL so the snapshot is consistent).
mkdir -p "$here/.pooled"
rsync -az bur.io:/mnt/mtgctl_data/data/mtgctl.db bur.io:/mnt/mtgctl_data/data/mtgctl.db-wal "$here/.pooled/"
python3 "$here/build_ranges.py" "$here/.pooled/mtgctl.db"
# New Legacy Data Collection sheets into the stats site's LDC dataset (runs on the server).
ssh bur.io '~/mtgctl-web/bin/mtgctl-web --data /mnt/mtgctl_data/data ldc-sync --repo ~/mtgctl-web' 2>&1 | grep -v "post-quantum\|store now\|upgraded. See"
