#!/usr/bin/env bash
# Runs a command in the WordCraft test VM over SSH (port 2229). See run-vm.sh.
set -euo pipefail
DIR=${WC_VM_DIR:-/mnt/HaikuWork/tmp/wordcraft-vm}
exec ssh -i "$DIR/id_ed25519" -p 2229 -o IdentitiesOnly=yes -o BatchMode=yes \
    -o UserKnownHostsFile="$DIR/known_hosts" -o StrictHostKeyChecking=accept-new \
    -o ConnectTimeout=10 -o ServerAliveInterval=30 user@127.0.0.1 "$@"
