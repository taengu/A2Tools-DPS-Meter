#!/usr/bin/env bash
# Publish a release: wait for the Linux packages run the tag push started, stage
# every package, write the manifest's hashes, upload (upload.sh).
#   bash release-template/publish.sh [version]   (default: tauri.conf.json's)
# Stages into release-<version>/ (made from this folder) and prints progress
# all the way, so a stall shows: the CI wait reports each job as it finishes
# and gives up when the run fails or goes 15 minutes without a job finishing.
set -euo pipefail
# A new publish starts unstopped (stop-publish.sh stops it again).
rm -f "$HOME/.a2tools-publish-stop"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
V="${1:-$(python -c "import json,sys;print(json.load(open(sys.argv[1]))['version'])" "$(cygpath -w "$ROOT/src-tauri/tauri.conf.json" 2>/dev/null || echo "$ROOT/src-tauri/tauri.conf.json")")}"
export V
export CFAT="${CFAT:-$(cat ~/.a2tools-cfat)}"
DIR="$ROOT/release-$V"
mkdir -p "$DIR"
cp "$ROOT/release-template/upload.sh" "$DIR/"
cd "$DIR"
REPO=taengu/A2Tools-DPS-Meter
gh_() { timeout 15 gh "$@" -R "$REPO"; }

# The run the tag push started (a hand-started run shows as "main", whatever
# ref it was given: pass RUN=<id> in the environment for one of those).
RUN="${RUN:-}"

for i in $(seq 1 20); do
  [ -n "$RUN" ] && break
  RUN=$(gh_ run list --workflow arch-package.yml --limit 20 --json databaseId,headBranch,displayTitle \
    --jq "[.[] | select(.headBranch == \"v$V\")][0].databaseId // empty" 2>/dev/null || true)
  [ -n "$RUN" ] && break
  echo "no Linux packages run for v$V yet; is the tag pushed? (check $i)"; sleep 6
done
[ -n "$RUN" ] || { echo "no Linux run for v$V; push the tag (git push origin v$V)"; exit 1; }
echo "Linux run $RUN"

last_done=-1; last_change=$SECONDS
while :; do
  s=$(gh_ run view "$RUN" --json status,conclusion,jobs \
    --jq '"\(.status) \(if (.conclusion // "") == "" then "none" else .conclusion end) \([.jobs[] | select(.status == "completed")] | length)/\(.jobs | length)"' 2>/dev/null || echo "? ? ?")
  set -- $s
  if [ "$3" != "$last_done" ]; then
    echo "linux: $1, $3 jobs done ($((SECONDS / 60)) min)"; last_done=$3; last_change=$SECONDS
  fi
  [ "$1" = completed ] && break
  if [ $((SECONDS - last_change)) -gt 900 ]; then
    echo "linux: no job has finished in 15 minutes; stopping (gh run view $RUN)"; exit 1
  fi
  sleep 15
done
[ "$2" = success ] || { echo "Linux run did not pass ($2); not publishing"; exit 1; }

rm -rf dl rpm
for a in arch-package deb-rpm-packages rpm-repo; do
  timeout 300 gh run download "$RUN" -R "$REPO" -n "$a" -D "dl/$a"
done
echo "downloaded the Linux packages"
cp "$ROOT/src-tauri/target/release/bundle/msi/A2Tools-DPS-Meter_${V}_x64_en-US.msi" .
cp dl/arch-package/*.zst dl/deb-rpm-packages/* . && cp -r dl/rpm-repo/rpm . && cp "$ROOT/packaging/linux/a2tools.repo" .
# Since 2.0.87 the repository's rpm is signed (and repomd.xml with it): that
# signed rpm is the one published at every address, the hash's too.
cp "rpm/a2tools-dps-meter-${V}-1.x86_64.rpm" .
[ -f rpm/repodata/repomd.xml.asc ] || { echo "the repository has no repomd.xml.asc: not publishing"; exit 1; }
cp dl/rpm-repo/a2tools.asc . || { echo "the rpm-repo artifact has no a2tools.asc: not publishing"; exit 1; }
cmp a2tools.asc "$ROOT/packaging/linux/a2tools.asc"
[ -f latest-v2.json ] || cp "$ROOT/latest-v2.json" .
python - <<PY
import json,hashlib
h=lambda f: hashlib.sha256(open(f,'rb').read()).hexdigest()
m=json.load(open('latest-v2.json',encoding='utf-8'))
V='$V'
m['msiSha256']=h(f'A2Tools-DPS-Meter_{V}_x64_en-US.msi'); m['archSha256']=h(f'a2tools-dps-meter-{V}-1-x86_64.pkg.tar.zst'); m['debSha256']=h(f'a2tools-dps-meter_{V}_amd64.deb'); m['rpmSha256']=h(f'a2tools-dps-meter-{V}-1.x86_64.rpm')
assert m['version']==V, (m['version'], V)
open('latest-v2.json','w',encoding='utf-8').write(json.dumps(m,ensure_ascii=False,indent=2)+'\n')
print('manifest', V)
PY
# Signed with the update key (~/.a2tools-update-key): meters from 2.0.80 check it.
node "$ROOT/tools/sign-manifest.mjs" latest-v2.json
bash ./upload.sh
# The release notes in the meter (Settings → What's new): when the GitHub
# release is already out; otherwise gh-release.sh does it once it is.
if timeout 15 gh release view "v$V" -R "$REPO" >/dev/null 2>&1; then
  bash "$ROOT/release-template/changelog.sh"
else
  echo "no GitHub release v$V yet: bash release-template/gh-release.sh <notes.md> creates it and uploads the release notes"
fi
