#!/usr/bin/env bash
# Publish feeds/pairings.json to cdn.a2tools.app/feeds/pairings.json, which
# meters read at start and every 6 hours (src-tauri/src/pairings.rs). After a
# new Server Matchmaking notice: edit pairings.json (each server's "abyss" and
# "rift" opponent, both ways, and "from"), run `cargo test pairings` (it checks
# every pairing goes both ways), then this. Needs CFAT, as the release uploads
# (~/.a2tools-cfat) and release-plugins/r2-put.sh beside this repo's root.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"
: "${CFAT:=$(cat ~/.a2tools-cfat)}"
export CFAT
python -c "import json; f=json.load(open('pairings.json', encoding='utf-8')); assert f['servers'], 'empty'; print(len(f['servers']), 'servers, from', f['from'])"
source ../release-plugins/r2-put.sh
put pairings.json feeds/pairings.json application/json no-cache
