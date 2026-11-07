#!/usr/bin/env bash
# Put a judge slot back to the shipped demo data: sudo /opt/sheltersim/reset-slot.sh <n>
set -euo pipefail
n="${1:?usage: reset-slot.sh <slot> [--no-restart]}"
[[ $n =~ ^[0-9]+$ ]] || { echo "bad slot: $n" >&2; exit 1; }
slot=/var/lib/sheltersim/slot$n
[[ ${2:-} == --no-restart ]] || systemctl stop "sheltersim@$n" || true
rm -rf "$slot"
mkdir -p "$slot"
cp -a /opt/sheltersim/app/fixtures/demo_replay "$slot/demo_replay"
chown -R sheltersim:sheltersim "$slot"
[[ ${2:-} == --no-restart ]] || systemctl start "sheltersim@$n"
echo "slot $n reset"
