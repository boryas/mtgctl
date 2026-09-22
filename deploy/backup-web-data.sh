#!/usr/bin/env bash
# Pull the droplet's raw uploads and player registry to a local mirror. The per-player SQLite
# files are derived (rebuildable with one sync) and are excluded; what cannot be rebuilt is
# logs/, sessions/ and players.toml, plus each player's marks once the export exists.
set -euo pipefail
DEST=${1:-$HOME/backups/mtgctl-web}
mkdir -p "$DEST"
rsync -az --delete --exclude 'mtgctl.db*' bur.io:/mnt/mtgctl_data/data/ "$DEST"/
# Point-in-time copy of the volume itself (stands in for object-store versioning); keeps the last 4.
VOL=$(doctl compute volume list --format ID,Name --no-header | awk '$2=="mtgctl-data"{print $1}')
if [ -n "$VOL" ]; then
  doctl compute volume snapshot "$VOL" --snapshot-name "mtgctl-data-$(date +%Y%m%d)" >/dev/null
  doctl compute snapshot list --resource volume --format ID,Name,Created --no-header | awk '$2 ~ /^mtgctl-data-/' | sort -k3 | head -n -4 | awk '{print $1}' | xargs -r -n1 doctl compute snapshot delete -f
fi
echo "$(date -Is) mirrored to $DEST ($(du -sh "$DEST" | cut -f1))"
