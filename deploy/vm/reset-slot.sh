#!/usr/bin/env bash
# Put a judge slot back to the shipped demo data and, when setup.sh built it, the
# precomputed sample Project: sudo /opt/sheltersim/reset-slot.sh <n>
set -euo pipefail
n="${1:?usage: reset-slot.sh <slot> [--no-restart]}"
[[ $n =~ ^[0-9]+$ ]] || { echo "bad slot: $n" >&2; exit 1; }
slot=/var/lib/sheltersim/slot$n
sample=/var/lib/sheltersim/sample/projects/sample
[[ ${2:-} == --no-restart ]] || systemctl stop "sheltersim@$n" || true
rm -rf "$slot"
mkdir -p "$slot"
cp -a /opt/sheltersim/app/fixtures/demo_replay "$slot/demo_replay"
if [[ -d $sample ]]; then
  mkdir -p "$slot/demo_replay/runs/.projects"
  cp -a "$sample" "$slot/demo_replay/runs/.projects/sample"
fi
chown -R sheltersim:sheltersim "$slot"
[[ ${2:-} == --no-restart ]] || systemctl start "sheltersim@$n"
echo "slot $n reset"
