#!/bin/sh
set -e

# shytti install + pair (Mode 2)
# curl -sSL https://raw.githubusercontent.com/calibrae/shytti/main/install.sh | sudo bash

INSTALL_DIR="/opt/shytti"
BIN="$INSTALL_DIR/shytti"
CONFIG="$INSTALL_DIR/shytti.toml"
SERVICE="/etc/systemd/system/shytti.service"

# --- Detect platform ---
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    aarch64|arm64) ARCH="aarch64" ;;
    x86_64|amd64)  ARCH="x86_64" ;;
    *) echo "unsupported arch: $ARCH"; exit 1 ;;
esac

URL="https://github.com/calibrae/shytti/releases/latest/download/shytti-${OS}-${ARCH}"

# --- Install ---
echo "=> installing shytti to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"

echo "=> downloading shytti-${OS}-${ARCH}"
curl -fsSL "$URL" -o "$BIN"
chmod +x "$BIN"

# --- Config ---
if [ ! -f "$CONFIG" ]; then
    LISTEN_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || ipconfig getifaddr en0 2>/dev/null || echo "0.0.0.0")
    cat > "$CONFIG" <<EOF
[daemon]
listen = "0.0.0.0:7778"
EOF
    echo "=> wrote config to $CONFIG"
else
    echo "=> config exists, keeping $CONFIG"
fi

# --- systemd ---
cat > "$SERVICE" <<EOF
[Unit]
Description=shytti — shell orchestrator
After=network.target

[Service]
Type=simple
ExecStart=$BIN -c $CONFIG
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable shytti
echo "=> systemd service installed"

# --- Pair ---
echo ""
echo "============================================"
echo "  shytti installed. generating pair token..."
echo "============================================"
echo ""

# Start shytti in background briefly to generate and serve the pair token
# The pair command starts the daemon and prints the token
$BIN pair -c "$CONFIG" &
PAIR_PID=$!

# Wait for it to print the token (it outputs to stdout)
sleep 2

echo ""
echo "============================================"
echo "  paste the token above into hermytt admin"
echo "  once paired, ctrl+c and start the service:"
echo ""
echo "    sudo systemctl start shytti"
echo "============================================"
echo ""

# Wait for pairing to complete (hermytt connects, then we can ctrl+c)
wait $PAIR_PID 2>/dev/null || true
