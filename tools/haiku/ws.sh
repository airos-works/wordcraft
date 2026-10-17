#!/usr/bin/env bash
# Run a command on the Haiku/air/OS build workstation ("shredder"), where the
# Haiku port is built. Usage: tools/haiku/ws.sh 'command'   (stdin is forwarded)
set -euo pipefail
WC_WS_HOST=${WC_WS_HOST:-192.168.1.247}
WC_WS_USER=${WC_WS_USER:-user}
WC_WS_KEY=${WC_WS_KEY:-/mnt/HaikuWork/x399/ssh/workstation_ed25519}
exec ssh -i "$WC_WS_KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
    -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR \
    -o ConnectTimeout=15 -o ServerAliveInterval=30 -o ServerAliveCountMax=8 \
    "$WC_WS_USER@$WC_WS_HOST" "$@"
