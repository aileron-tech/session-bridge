#!/bin/sh
# Turns networking on in both laptops and pairs them through the control API:
# laptop-a asks laptop-b for a one-time code, reads the code laptop-b shows
# (its `lan` view), and submits it. Run from anywhere after `compose up`.
set -eu
cd "$(dirname "$0")"

ctl() { docker compose -f compose.yml exec -T -u dev "$1" /demo/bin/sbctl "$2"; }
field() { sed -n "s/.*\"$1\": \"\([^\"]*\)\".*/\1/p" | head -n 1; }

echo "Networking on"
ctl laptop-a '{"method":"network","enabled":true,"port":7300,"name":"laptop-a"}' >/dev/null
ctl laptop-b '{"method":"network","enabled":true,"port":7300,"name":"laptop-b"}' >/dev/null

# By name once mDNS has announced laptop-b, otherwise by address.
target=10.228.0.12:7300
for _ in $(seq 1 10); do
    if ctl laptop-a '{"method":"lan"}' | grep -q '"host": "laptop-b"'; then
        target=laptop-b
        break
    fi
    sleep 1
done
echo "laptop-a asks $target for a code"
id=$(ctl laptop-a "{\"method\":\"pair\",\"host\":\"$target\"}" | field id)
[ -n "$id" ] || { echo "Pairing did not start"; exit 1; }

code=
for _ in $(seq 1 10); do
    code=$(ctl laptop-b '{"method":"lan"}' | field code)
    [ -n "$code" ] && break
    sleep 1
done
[ -n "$code" ] || { echo "laptop-b shows no code"; exit 1; }
echo "laptop-b shows code $code"

ctl laptop-a "{\"method\":\"pair_code\",\"id\":\"$id\",\"code\":\"$code\"}"
