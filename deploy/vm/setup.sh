#!/usr/bin/env bash
# Set up judge slots of 避難所IF！ on a fresh Ubuntu 24.04 VM (docs/vm-deploy.md).
#
#   sudo bash deploy/vm/setup.sh <public-ip> [slots]
#
# The app is installed from the commit this script was checked out at (override with
# REF=<commit or origin/branch>), so that commit must be pushed to REPO_URL.
#
# Each slot is its own `sheltersim web` process with its own data directory, served at
# https://sN.<ip-with-dashes>.sslip.io behind one Basic-auth login. All slots share one
# LLM usage ledger (XDG_STATE_HOME), so the default $50 cap holds for the whole VM.
# Re-running the script is safe: it keeps the API key, the password and slot data.
set -euo pipefail

PUBLIC_IP="${1:?usage: sudo bash setup.sh <public-ip> [slots]}"
SLOTS="${2:-6}"
REPO_URL="${REPO_URL:-https://github.com/mizukidayo1025/HinanjoIF-Shelter_Sim-.git}"
BASIC_USER="${BASIC_USER:-judge}"

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
"$VENV/bin/python" -c "import sheltersim.native as n; assert n.available(), 'kernels not loaded'"

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
  BASIC_PASS="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 16)"
  printf '%s\n%s\n' "$BASIC_USER" "$BASIC_PASS" > "$ETC/basic-auth"
  chmod 600 "$ETC/basic-auth"
fi
BASIC_USER="$(sed -n 1p "$ETC/basic-auth")"
BASIC_PASS="$(sed -n 2p "$ETC/basic-auth")"
BASIC_HASH="$(caddy hash-password --plaintext "$BASIC_PASS")"

echo "== slots 1..$SLOTS"
install -m 755 "$HERE/reset-slot.sh" /opt/sheltersim/reset-slot.sh
install -m 644 "$HERE/sheltersim@.service" /etc/systemd/system/sheltersim@.service
for n in $(seq 1 "$SLOTS"); do
  echo "PORT=$((8800 + n))" > "$ETC/slot$n.env"
  if [[ ! -d $DATA/slot$n/demo_replay ]]; then
    /opt/sheltersim/reset-slot.sh "$n" --no-restart
  fi
done
chown -R sheltersim:sheltersim "$DATA"

echo "== landing page and Caddy"
mkdir -p /opt/sheltersim/landing
python3 "$HERE/render_landing.py" "$DOMAIN" "$SLOTS" > /opt/sheltersim/landing/index.html
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
