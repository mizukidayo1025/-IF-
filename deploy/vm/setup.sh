#!/usr/bin/env bash
# Set up judge slots of 避難所IF！ on a fresh Ubuntu 24.04 VM (docs/vm-deploy.md).
#
#   sudo bash deploy/vm/setup.sh [public-ip [slots]]
#
# Without public-ip, the VM's external IP is read from the GCP metadata server.
#
# The app is installed from the commit this script was checked out at (override with
# REF=<commit or origin/branch>), so that commit must be pushed to REPO_URL.
#
# Each slot is its own `sheltersim web` process with its own data directory, served at
# https://sN.<ip-with-dashes>.sslip.io behind one Basic-auth login. All slots share one
# LLM usage ledger (XDG_STATE_HOME), so the default $50 cap holds for the whole VM.
# Re-running the script is safe: it keeps the API key, the password and slot data.
#
# The precomputed sample Project (scripts/build_project_sample.py) is built into
# /var/lib/sheltersim/sample, again only when its result would change, and copied into
# each slot that lacks it.
# SAMPLE=0 skips it; SAMPLE=rebuild builds it again even when it is current.
set -euo pipefail

METADATA=http://metadata.google.internal/computeMetadata/v1/instance/network-interfaces/0
PUBLIC_IP="${1:-}"
if [[ -z $PUBLIC_IP ]]; then
  PUBLIC_IP="$(curl -sf --max-time 5 -H 'Metadata-Flavor: Google' \
    "$METADATA/access-configs/0/external-ip" || true)"
  [[ -n $PUBLIC_IP ]] || {
    echo "could not read the external IP from GCP; run: sudo bash setup.sh <public-ip> [slots]" >&2
    exit 1
  }
  echo "== external IP from GCP metadata: $PUBLIC_IP"
fi
SLOTS="${2:-6}"
REPO_URL="${REPO_URL:-https://github.com/mizukidayo1025/HinanjoIF-Shelter_Sim-.git}"
BASIC_USER="${BASIC_USER:-judge}"
SAMPLE="${SAMPLE:-1}"

APP=/opt/sheltersim/app
VENV=/opt/sheltersim/venv
DATA=/var/lib/sheltersim
ETC=/etc/sheltersim
DOMAIN="${PUBLIC_IP//./-}.sslip.io"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [[ -z ${REF:-} ]]; then
  REF="$(git -c safe.directory='*' -C "$HERE" rev-parse HEAD 2>/dev/null || echo origin/main)"
fi

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
[[ $PUBLIC_IP =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad IP: $PUBLIC_IP" >&2; exit 1; }

echo "== packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q git python3-venv python3-pip build-essential curl gnupg debian-keyring \
  debian-archive-keyring apt-transport-https
if ! command -v caddy >/dev/null; then
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
    | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
    > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -q
  apt-get install -y -q caddy
fi

echo "== app ($REF)"
id sheltersim >/dev/null 2>&1 || useradd --system --home "$DATA" --shell /usr/sbin/nologin sheltersim
mkdir -p /opt/sheltersim "$DATA/state" "$ETC"
if [[ ! -d $APP/.git ]]; then
  git clone -q "$REPO_URL" "$APP"
fi
git -C "$APP" fetch -q origin
git -C "$APP" checkout -q --detach "$REF"
[[ -x $VENV/bin/python ]] || python3 -m venv "$VENV"
"$VENV/bin/pip" install -q --upgrade pip
"$VENV/bin/pip" install -q "${APP}[llm,plan-import]"

echo "== Rust kernels"
export RUSTUP_HOME=/opt/sheltersim/rustup CARGO_HOME=/opt/sheltersim/cargo
if [[ ! -x $CARGO_HOME/bin/cargo ]]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --no-modify-path
fi
export PATH="$CARGO_HOME/bin:$PATH"
rustup -q toolchain install stable --profile minimal
"$VENV/bin/pip" install -q "maturin>=1.7,<2"
rm -rf /opt/sheltersim/native-dist
"$VENV/bin/maturin" build -q --release -m "$APP/native/Cargo.toml" -i "$VENV/bin/python" \
  --out /opt/sheltersim/native-dist
"$VENV/bin/pip" install -q --force-reinstall --no-deps --no-index \
  --find-links /opt/sheltersim/native-dist sheltersim-native
"$VENV/bin/python" -c "import sheltersim.native"

echo "== settings"
if [[ ! -f $ETC/env ]]; then
  install -m 600 /dev/null "$ETC/env"
  cat > "$ETC/env" <<'EOF'
# Put the Anthropic API key after the = sign, then: sudo systemctl restart 'sheltersim@*'
ANTHROPIC_API_KEY=
SHELTERSIM_LLM_MODE=auto
SHELTERSIM_LLM_ERROR_POLICY=fallback
SHELTERSIM_OPUS_MODEL_ID=claude-opus-5-5
SHELTERSIM_SONNET_MODEL_ID=claude-sonnet-5-5
SHELTERSIM_HAIKU_MODEL_ID=claude-haiku-5-5
SHELTERSIM_PREPARE_MODEL_FAMILY=opus
SHELTERSIM_INTERVENTION_MODEL_FAMILY=opus
SHELTERSIM_REVIEW_MODEL_FAMILY=opus
SHELTERSIM_LLM_MAX_API_CALLS=30
SHELTERSIM_LLM_MAX_CONCURRENCY=2
SHELTERSIM_LLM_BUDGET_USD=10.00
SHELTERSIM_LOG_LEVEL=summary
PYTHONUNBUFFERED=1
EOF
fi
chown root:sheltersim "$ETC/env"
chmod 640 "$ETC/env"

if [[ ! -f $ETC/basic-auth ]]; then
  # Not `tr </dev/urandom | head`: under pipefail, tr's SIGPIPE ends the script silently.
  BASIC_PASS="$(python3 -c 'import secrets, string
print("".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(16)))')"
  printf '%s\n%s\n' "$BASIC_USER" "$BASIC_PASS" > "$ETC/basic-auth"
  chmod 600 "$ETC/basic-auth"
fi
BASIC_USER="$(sed -n 1p "$ETC/basic-auth")"
BASIC_PASS="$(sed -n 2p "$ETC/basic-auth")"
BASIC_HASH="$(caddy hash-password --plaintext "$BASIC_PASS")"

# The precomputed sample: a 7-day Run of the new screen's example Scenario and its
# Day 4 toilet outage branch, which the new screen opens without computing. It is built as the sheltersim user,
# offline, without the API key and away from the shared usage ledger, and built again only
# when a build now would give another result (the builder's --check: plan, Scenario hash,
# model signatures), or with SAMPLE=rebuild (docs/vm-deploy.md).
SAMPLE_DIR=$DATA/sample
sample_builder() {
  local dir=$1
  shift
  runuser -u sheltersim -- env -i -C "$dir" PATH=/usr/bin:/bin HOME="$DATA" \
    PYTHONUNBUFFERED=1 SHELTERSIM_LLM_MODE=offline XDG_STATE_HOME="$dir/state" \
    SHELTERSIM_PROJECTS_DIR="$dir/projects" SHELTERSIM_LLM_CACHE_PATH="$dir/llm-tape.json" \
    "$VENV/bin/python" "$APP/scripts/build_project_sample.py" "$dir/projects" \
    --summary "$dir/sample-build.json" "$@"
}
if [[ $SAMPLE == 0 ]]; then
  rm -rf "$SAMPLE_DIR"
else
  SAMPLE_STATE=missing
  if [[ $SAMPLE != rebuild && -d $SAMPLE_DIR ]]; then
    SAMPLE_STATE="$(sample_builder "$SAMPLE_DIR" --check || true)"
    SAMPLE_STATE="${SAMPLE_STATE:-rebuild: the check failed}"
    rm -f "$SAMPLE_DIR/app-commit"
  fi
  echo "== precomputed sample: ${SAMPLE_STATE}"
  if [[ $SAMPLE_STATE != current ]]; then
    echo "== precomputed sample (about 5 minutes, 4 GB of memory)"
    rm -rf "$SAMPLE_DIR.new"
    install -d -o sheltersim -g sheltersim "$SAMPLE_DIR.new"
    sample_builder "$SAMPLE_DIR.new" >/dev/null
    rm -rf "$SAMPLE_DIR.new/state" "$SAMPLE_DIR.new/llm-tape.json"
    rm -rf "$SAMPLE_DIR"
    mv "$SAMPLE_DIR.new" "$SAMPLE_DIR"
  fi
fi
SAMPLE_GB=0
if [[ -d $SAMPLE_DIR ]]; then
  SAMPLE_GB="$(du -s -B 1000000000 "$SAMPLE_DIR" | cut -f1)"
fi

# Each slot may keep this much saved data, so that all slots together cannot fill the disk:
# the disk less a reserve for the system, the app and its build, and less the sample's
# template, shared by the slots. A slot's own copy of the sample counts toward its share.
# Set STORAGE_GB=<n> to choose it; run setup.sh again after resizing the disk.
DISK_GB="$(df --output=size -B 1000000000 "$DATA" | tail -n 1 | tr -d ' ')"
STORAGE_GB="${STORAGE_GB:-$(( (DISK_GB - 15 - SAMPLE_GB) / SLOTS ))}"
[[ $STORAGE_GB =~ ^[0-9]+$ && $STORAGE_GB -gt $SAMPLE_GB ]] || {
  echo "not enough disk for $SLOTS slots (${DISK_GB} GB, sample ${SAMPLE_GB} GB);" \
    "set STORAGE_GB, use fewer slots or skip the sample with SAMPLE=0" >&2
  exit 1
}

echo "== slots 1..$SLOTS (${STORAGE_GB} GB of saved data each, disk ${DISK_GB} GB)"
install -m 755 "$HERE/reset-slot.sh" /opt/sheltersim/reset-slot.sh
install -m 644 "$HERE/sheltersim@.service" /etc/systemd/system/sheltersim@.service
for n in $(seq 1 "$SLOTS"); do
  printf 'PORT=%s\nSTORAGE_GB=%s\n' "$((8800 + n))" "$STORAGE_GB" > "$ETC/slot$n.env"
  projects=$DATA/slot$n/demo_replay/runs/.projects
  if [[ ! -d $DATA/slot$n/demo_replay ]]; then
    /opt/sheltersim/reset-slot.sh "$n" --no-restart
  elif [[ -d $SAMPLE_DIR && ! -e $projects/sample ]]; then
    # Copied beside the store and moved in, so a running slot never lists half a copy.
    rm -rf "$DATA/slot$n/sample.copy"
    cp -a "$SAMPLE_DIR/projects/sample" "$DATA/slot$n/sample.copy"
    mkdir -p "$projects"
    mv "$DATA/slot$n/sample.copy" "$projects/sample"
  fi
done
chown -R sheltersim:sheltersim "$DATA"

echo "== landing page and Caddy"
mkdir -p /opt/sheltersim/landing
SAMPLE_SUMMARY=()
if [[ -f $SAMPLE_DIR/sample-build.json ]]; then
  SAMPLE_SUMMARY=("$SAMPLE_DIR/sample-build.json")
fi
python3 "$HERE/render_landing.py" "$DOMAIN" "$SLOTS" "${SAMPLE_SUMMARY[@]}" \
  > /opt/sheltersim/landing/index.html
{
  echo "(auth) {"
  echo "  basic_auth {"
  echo "    $BASIC_USER $BASIC_HASH"
  echo "  }"
  echo "}"
  echo "$DOMAIN {"
  echo "  import auth"
  echo "  root * /opt/sheltersim/landing"
  echo "  file_server"
  echo "}"
  for n in $(seq 1 "$SLOTS"); do
    echo "s$n.$DOMAIN {"
    echo "  import auth"
    # Judges use the new screen only: the app's / and the older screens (/demo, /admin)
    # all go to /demo-v2 with the same query (?project=...&run=...). The API and the new
    # screen's files pass through.
    echo "  @older path / /demo /admin /index.html"
    echo "  redir @older /demo-v2{?query} 302"
    echo "  reverse_proxy 127.0.0.1:$((8800 + n))"
    echo "}"
  done
} > /etc/caddy/Caddyfile
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null

systemctl daemon-reload
for n in $(seq 1 "$SLOTS"); do
  systemctl enable -q "sheltersim@$n"
  systemctl restart "sheltersim@$n"
done
systemctl enable -q caddy
systemctl reload caddy || systemctl restart caddy

echo
echo "Done. Judges open: https://$DOMAIN/"
echo "  user: $BASIC_USER"
echo "  pass: $BASIC_PASS"
grep -q '^ANTHROPIC_API_KEY=.\+' "$ETC/env" \
  || echo "LLM is offline until you set ANTHROPIC_API_KEY in $ETC/env and restart the slots."
