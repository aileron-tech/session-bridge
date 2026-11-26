#!/bin/sh
# Connects laptop-a to remote-box, which sits on another network and is
# reached only by SSH: a demo-only key for laptop-a, the remote bridge on
# loopback (network on --local-only), an SSH tunnel kept by laptop-a's bridge,
# and pairing through it with the code remote-box shows. Run after pair.sh.
set -eu
cd "$(dirname "$0")"

ctl() { docker compose -f compose.yml exec -T -u dev "$1" /demo/bin/sbctl "$2"; }
as_dev() { docker compose -f compose.yml exec -T -u dev "$1" sh -c "$2"; }
field() { sed -n "s/.*\"$1\": \"\([^\"]*\)\".*/\1/p" | head -n 1; }

echo "SSH key for laptop-a, authorized on remote-box"
as_dev laptop-a 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && { [ -f ~/.ssh/id_ed25519 ] || ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/id_ed25519; }
printf "Host remote-box\n  User dev\n  StrictHostKeyChecking accept-new\n" > ~/.ssh/config && chmod 600 ~/.ssh/config'
key=$(as_dev laptop-a 'cat ~/.ssh/id_ed25519.pub')
as_dev remote-box "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo '$key' > ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"

echo "remote-box: bridge on loopback only"
ctl remote-box '{"method":"network","enabled":true,"port":7300,"name":"remote-box","local_only":true}' >/dev/null

echo "laptop-a: remote host remote-box over SSH"
ctl laptop-a '{"method":"add_remote","host":"remote-box","port":7300}' >/dev/null
for _ in $(seq 1 30); do
    ctl laptop-a '{"method":"remotes"}' | grep -q '"state": "connected"' && break
    sleep 1
done
ctl laptop-a '{"method":"remotes"}' | grep '"state"'

id=$(ctl laptop-a '{"method":"pair","host":"remote-box"}' | field id)
[ -n "$id" ] || { echo "Pairing did not start"; exit 1; }
code=
for _ in $(seq 1 10); do
    code=$(ctl remote-box '{"method":"lan"}' | field code)
    [ -n "$code" ] && break
    sleep 1
done
[ -n "$code" ] || { echo "remote-box shows no code"; exit 1; }
echo "remote-box shows code $code"
ctl laptop-a "{\"method\":\"pair_code\",\"id\":\"$id\",\"code\":\"$code\"}"
