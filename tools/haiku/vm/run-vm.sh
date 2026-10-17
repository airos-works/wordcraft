#!/usr/bin/env bash
# Starts a throwaway Haiku test VM for the air/OS port (VNC 127.0.0.1:5909, SSH 2229).
#
# It boots TasAmp's Haiku R1/beta6 disk (../tasamp/.vm/work.qcow2, override with
# WC_VM_BASE) read-only through a qcow2 overlay in WC_VM_DIR (default
# /mnt/HaikuWork/tmp/wordcraft-vm), which also holds the SSH key, QMP socket and pid.
# QEMU locks the base: TasAmp's own VM can't run at the same time.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
BASE=${WC_VM_BASE:-$(cd "$ROOT/.." && pwd)/tasamp/.vm/work.qcow2}
DIR=${WC_VM_DIR:-/mnt/HaikuWork/tmp/wordcraft-vm}
OVERLAY=$DIR/overlay.qcow2
STAMP=$DIR/base.stamp
mkdir -p "$DIR"
if [[ -f $DIR/qemu.pid ]] && kill -0 "$(cat "$DIR/qemu.pid")" 2>/dev/null; then
    echo 'The WordCraft test VM is already running.'
    exit 0
fi
stamp=$(stat -c '%s %Y' "$BASE")
if [[ -f $OVERLAY && "$(cat "$STAMP" 2>/dev/null)" != "$stamp" ]]; then
    echo 'Base disk changed; starting from a fresh overlay.'
    rm -f "$OVERLAY"
fi
if [[ ! -f $OVERLAY ]]; then
    qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
    echo "$stamp" > "$STAMP"
fi
rm -f "$DIR/qmp.sock"
exec qemu-system-x86_64 -enable-kvm -cpu host -m "${WC_VM_MEMORY:-6144}" -smp "${WC_VM_CPUS:-6}" \
    -drive file="$OVERLAY",format=qcow2,if=ide,index=0 \
    -nic user,model=e1000,hostfwd=tcp:127.0.0.1:2229-:22 \
    -device qemu-xhci -device usb-tablet -vga std -display none \
    -vnc 127.0.0.1:9 -qmp unix:"$DIR/qmp.sock",server=on,wait=off \
    -pidfile "$DIR/qemu.pid" -serial file:"$DIR/serial.log" -daemonize
