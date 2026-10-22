#!/bin/sh
# Refresh the EW prep page's data files (meta.json, ranges.json, sbmaps.json) from the stats site.
# Publishing the page afterwards is a separate step (the Artifact republish of prep/index.html + data files).
set -e
DAYS=${1:-28}
here=$(cd "$(dirname "$0")" && pwd)
data=/mnt/mtgctl_data/data
quiet() { grep -v "post-quantum\|store now\|upgraded. See" || true; }
# Catch up the site's LDC dataset (sheets + goldfish Challenge lists) now; its daily timer runs at 09:00 UTC.
ssh bur.io "~/mtgctl-web/bin/mtgctl-web --data $data ldc-sync --repo ~/mtgctl-web" 2>&1 | quiet
# The pooled game-log DB (copied with its WAL so the snapshot is consistent) and the LDC list cache.
mkdir -p "$here/.pooled/ldc"
rsync -az "bur.io:$data/mtgctl.db" "bur.io:$data/mtgctl.db-wal" "$here/.pooled/" 2>&1 | quiet
rsync -az --delete "bur.io:$data/ldc/pages" "bur.io:$data/ldc/lists" "$here/.pooled/ldc/" 2>&1 | quiet
python3 "$here/build_meta.py" "$DAYS"
python3 "$here/build_ranges.py" "$here/.pooled/mtgctl.db"
python3 "$here/sbmap.py" export "$here/sbmaps/tempo-ew-current.toml" "$here/sbmaps/tempo-ew-draft-b.toml" "$here/sbmaps/tempo-eureka.toml" --meta "$here/sbmaps/expected-meta.csv" -o "$here/sbmaps.json"
