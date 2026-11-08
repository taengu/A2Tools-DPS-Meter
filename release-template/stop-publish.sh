#!/usr/bin/env bash
# Stop a publish for certain. Stopping the shell that started publish.sh
# leaves publish.sh, upload.sh and their uploads running (2.0.93 went live
# that way after "stopping" it), and Git Bash's ps doesn't list them. So:
#  1. the stop file, which every upload checks first (release-plugins/r2-put.sh);
#  2. every process running publish.sh, upload.sh, r2-put.sh or the CI wait, killed
#     through Windows, until none is left;
#  3. what the CDN's update manifest says now.
set -uo pipefail
touch "$HOME/.a2tools-publish-stop"
echo "stop file: $HOME/.a2tools-publish-stop"
for i in 1 2 3 4 5 6; do
  left="$(powershell -NoProfile -Command "\$p = Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -match 'publish\.sh|upload\.sh|upload-plugins\.sh|r2-put|aws(\.exe)? s3api put-object|gh run (watch|view|download)' -and \$_.CommandLine -notmatch 'Get-CimInstance|stop-publish' }; \$p | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue }; (\$p | Measure-Object).Count")"
  left="${left//[$'\r\n ']/}"
  [ "${left:-0}" = 0 ] && break
  echo "killed $left process(es); checking again"
  sleep 1
done
[ "${left:-0}" = 0 ] && echo "no publish process left" || { echo "FAILED: processes still running" >&2; exit 1; }
echo "CDN manifest now: $(curl -s --max-time 20 https://cdn.a2tools.app/latest-v2.json | grep -m1 '"version"')"
