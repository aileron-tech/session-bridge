#!/bin/sh
# Container start (root): cron for scheduled messages, then the laptop as
# the unprivileged user "dev".
set -eu
cron
# A remote host (SSHD=1) accepts ssh with keys only; its bridge listens on
# loopback and is reached through the tunnel laptop-a keeps.
if [ "${SSHD:-0}" = 1 ]; then
    ssh-keygen -A >/dev/null
    /usr/sbin/sshd
fi
exec runuser -u dev -- /demo/bin/laptop.sh
