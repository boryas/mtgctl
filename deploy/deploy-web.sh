#!/usr/bin/env bash
# Build mtgctl-web for the droplet's older glibc (Debian bullseye container) and push binary,
# pages and marker rules to bur.io:~/mtgctl-web. Data and players.toml stay put.
set -euo pipefail
cd "$(dirname "$0")/.."
docker run --rm -v "$PWD":/src -w /src -v mtgctl-cargo-cache:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/src/target-bullseye rust:1.88-bullseye cargo build --release --bin mtgctl-web
H=bur.io
ssh "$H" 'mkdir -p ~/mtgctl-web/{bin,web,definitions/mtgo,deploy,data} ~/.config/systemd/user'
rsync -az target-bullseye/release/mtgctl-web "$H":mtgctl-web/bin/mtgctl-web.new
rsync -az --delete web/ "$H":mtgctl-web/web/
rsync -az deploy/windows/ "$H":mtgctl-web/web/windows/
rsync -az definitions/mtgo/*.toml "$H":mtgctl-web/definitions/mtgo/
rsync -az deploy/mtgo_cards_seed.sql "$H":mtgctl-web/deploy/
rsync -az deploy/mtgctl-web.service "$H":.config/systemd/user/mtgctl-web.service
rsync -az deploy/ldc-sync.service deploy/ldc-sync.timer "$H":.config/systemd/user/
ssh "$H" 'mv ~/mtgctl-web/bin/mtgctl-web.new ~/mtgctl-web/bin/mtgctl-web && systemctl --user daemon-reload && systemctl --user restart mtgctl-web && systemctl --user enable --now ldc-sync.timer && sleep 1 && systemctl --user is-active mtgctl-web'
