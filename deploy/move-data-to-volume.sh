#!/usr/bin/env bash
# One-time: copy ~/mtgctl-web/data onto the DO volume and repoint the service at it
# (the unit's --data is /mnt/mtgctl_data/data). Old directory is kept as data.moved.
set -euo pipefail
H=bur.io
rsync -az "$(dirname "$0")/mtgctl-web.service" "$H":.config/systemd/user/mtgctl-web.service
ssh "$H" 'set -e; test -w /mnt/mtgctl_data || { echo "/mnt/mtgctl_data not writable by $(id -un)"; exit 1; }
  systemctl --user stop mtgctl-web
  if [ -d ~/mtgctl-web/data ]; then
    rsync -a ~/mtgctl-web/data/ /mnt/mtgctl_data/data/
    mv ~/mtgctl-web/data ~/mtgctl-web/data.moved
  fi
  systemctl --user daemon-reload; systemctl --user start mtgctl-web; sleep 1; systemctl --user is-active mtgctl-web
  du -sh /mnt/mtgctl_data/data; df -h /mnt/mtgctl_data | tail -1
  TOK=$(grep -oE "token = \"[0-9a-f]+\"" /mnt/mtgctl_data/data/players.toml | head -1 | cut -d\" -f2)
  curl -s -H "Authorization: Bearer $TOK" http://127.0.0.1:8090/api/me; echo
  curl -s -H "Authorization: Bearer $TOK" "http://127.0.0.1:8090/api/stats?by=my_concept" | head -c 200; echo'
