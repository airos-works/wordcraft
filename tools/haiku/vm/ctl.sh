#!/usr/bin/env bash
# Sends control-channel requests (JSON objects without "id"/"key", one per argument) to
# WordCraft running in the test VM with --control 7981, and prints the replies.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
reqs=""
n=1
for r in "$@"; do
    reqs+="${r%\}},\"id\":$n,\"key\":\"KEY\"}"$'\n'
    n=$((n + 1))
done
printf '%s' "$reqs" | bash "$HERE/haiku.sh" 'key=$(cat ~/config/settings/WordCraft/control-key.local-7981); sed "s/KEY/$key/" | nc -q 3 127.0.0.1 7981 2>/dev/null || sed "s/KEY/$key/" | nc 127.0.0.1 7981'
