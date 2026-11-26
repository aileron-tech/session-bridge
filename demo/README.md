# Demo environment

Three simulated machines in Docker: two laptops on one office network (they
find each other by mDNS) and a remote host on another network that only
laptop-a reaches, by SSH. Each runs a real session-bridge daemon (Linux build)
with scripted stand-in agents, for recording the admin page: channels,
@mentions, roles, the default recipient, scheduled messages through cron,
pairing across laptops, and a remote host through an SSH tunnel.

| Laptop | Sessions | Admin page on the Mac |
|---|---|---|
| laptop-a | lead (Claude), reviewer (Codex) in `shop` | `127.0.0.1:7101` |
| laptop-b | frontend (Claude), backend (Codex) in `shop`; docs (Claude) in `handbook` | `127.0.0.1:7102` |
| remote-box | ops (Claude) in `shop`; sshd, bridge on 127.0.0.1 only | `127.0.0.1:7103` |

The team ships "checkout v2" of a web shop. The stand-ins answer with short
Markdown (status tables, checklists), depending on what they receive:

- `@lead` with a plan request: lead posts a checklist, asks reviewer for a
  review and frontend (on laptop-b) about the cart page; frontend asks
  backend about the API; when both answers are in, lead reports in the
  channel.
- `status` or `standup`, `@channel`, a scheduled message, or a direct message:
  each session posts its status.

Everything is invented: no real names, paths, customers or tokens.

## Start

From the repository root:

```sh
docker compose -f demo/compose.yml up --build -d
demo/pair.sh      # networking on, laptop-a pairs with laptop-b (mDNS)
demo/remote.sh    # laptop-a reaches remote-box by SSH and pairs through the tunnel
demo/seed.sh      # optional: default recipient, a schedule, roles, the opening post
```

The first build compiles the bridge in release mode (a few minutes).

## Open the admin pages

Each laptop prints its page address on start and keeps it in
`~/admin-url.txt`:

```sh
docker compose -f demo/compose.yml exec -u dev laptop-a cat /home/dev/admin-url.txt
docker compose -f demo/compose.yml exec -u dev laptop-b cat /home/dev/admin-url.txt
```

Open the printed `http://127.0.0.1:7101/#token=...` (and `7102`) in a
browser. The page checks that it is addressed as its own `127.0.0.1:<port>`
inside the container; `bin/admin_proxy.py` forwards the published port to it
and rewrites Host and Origin. The page token is still required.

## Scheduled messages

Each laptop runs cron. A schedule saved on the page becomes a line in the
`dev` user's crontab; check it with:

```sh
docker compose -f demo/compose.yml exec -u dev laptop-a crontab -l
```

Use "Run now" on the page to post it without waiting. The containers run
in UTC.

## Look inside

```sh
docker compose -f demo/compose.yml logs -f              # daemon and agents
docker compose -f demo/compose.yml exec -u dev laptop-a session-bridge list
docker compose -f demo/compose.yml exec -u dev laptop-a /demo/bin/sbctl '{"method":"lan"}'
```

## Stop and remove

```sh
docker compose -f demo/compose.yml down --rmi local     # containers, network, image
```

The demo uses no volumes; all state ends with its containers.

## How the stand-ins work

- `agents/agent.py --kind claude` writes `~/.claude/sessions/<pid>.json` and
  serves a Unix-socket inbox (one JSON frame per connection). It confirms
  each frame with a `peer_message_status` control frame and answers through
  the frame's `from` return socket, like a Claude session.
- `agents/agent.py --kind codex` keeps a rollout file under
  `~/.codex/sessions/` open (the bridge finds Codex sessions with `lsof`),
  names itself in `~/.codex/session_index.jsonl`, reads what `bin/codex` (the
  stand-in for `codex queue`, set as `SESSION_BRIDGE_CODEX_BIN`) drops in its
  inbox folder, and answers with `session-bridge send`.
- The bridge runs as a Linux build: discovery uses `/usr/bin/lsof` and
  `/usr/bin/sqlite3` there; the menu-bar icon and LaunchAgent are macOS only.
