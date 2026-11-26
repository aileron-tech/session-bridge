#!/bin/sh
# One simulated laptop: the bridge daemon, the stand-in agents named in
# $AGENTS ("name:claude|codex:project ..."), and the admin page proxy.
set -eu
cd "$HOME"
export SESSION_BRIDGE_NOTIFY=0
export SESSION_BRIDGE_CODEX_BIN=/demo/bin/codex
export PATH="/demo/bin:$PATH"

session-bridge daemon &
daemon=$!
for _ in $(seq 1 50); do
    session-bridge status >/dev/null 2>&1 && break
    sleep 0.2
done
session-bridge status >/dev/null

for agent in ${AGENTS:-}; do
    name=${agent%%:*}
    rest=${agent#*:}
    kind=${rest%%:*}
    project=${rest#*:}
    python3 /demo/agents/agent.py --kind "$kind" --name "$name" --project "$project" &
done

python3 /demo/bin/admin_proxy.py --public-port "${ADMIN_PUBLIC_PORT:-7100}" &

# The daemon is the laptop: when it ends, the container ends.
wait "$daemon"
