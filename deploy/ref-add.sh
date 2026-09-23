#!/usr/bin/env bash
# Seed one reference decklist on the droplet from a local decklist file.
# usage: deploy/ref-add.sh <file> --archetype Doomsday --deck "UB Tempo DD" --name "Legacy Challenge 32 2026-09-06 5th, boryas" --date 2026-09-06 [--source URL] [--by name]
set -euo pipefail
[ $# -ge 2 ] || { echo "usage: $0 <decklist file> --archetype A --deck D --name N --date YYYY-MM-DD [--source URL] [--by who]"; exit 1; }
f=$1; shift
scp -q "$f" bur.io:/tmp/ref.txt
ssh bur.io "cd ~/mtgctl-web && ./bin/mtgctl-web --data /mnt/mtgctl_data/data ref-add $(printf '%q ' "$@") /tmp/ref.txt; rm -f /tmp/ref.txt" 2>&1 | grep -v "post-quantum\|store now\|openssh.com/pq"
