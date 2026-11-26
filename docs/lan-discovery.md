# LAN discovery, pairing and admin (REQ-017, REQ-018, REQ-019, REQ-020)

Status: implemented.

## Problem

The user runs Codex and Claude sessions on several of their own laptops on one
local network. Sessions on one laptop could not list or message sessions on
another: the bridge accepted only same-user, same-machine connections (Unix
socket or named pipe, REQ-016).

Scope: one network segment, the user's own laptops. Out of scope: phones,
other people's machines, routing across networks, NAT traversal, QR codes.

## Decisions

- Discovery: Zeroconf (mDNS + DNS-SD, RFC 6762/6763) with the pure Rust
  `mdns-sd` crate. The `zeroconf` crate was rejected: on Windows it needs
  Apple Bonjour installed.
- Pairing works like adding a printer: every bridge announces itself, the user
  picks one, the chosen laptop shows a one-time code, the user types it on the
  laptop that chose (Bluetooth passkey entry). Typing the right code proves
  the person can see the chosen laptop's screen; that is its consent.
- The code is verified with SPAKE2: a 6-digit code is safe only if it cannot
  be tested offline. A man in the middle gets one online guess per try.
- Connections use Noise `XX_25519_ChaChaPoly_BLAKE2s` (`snow`, pure Rust
  crypto) with pinned static keys. TLS (rustls) was dropped: its `ring`
  provider needs a C toolchain to cross-build the Windows target, and pinned
  self-signed certificates would add certificate handling without benefit.
- IPv4 only: the listener binds `0.0.0.0`, so discovery ignores IPv6
  addresses and does not announce on IPv6 interfaces. A host is listed only
  once it has an IPv4 address (an IPv6 link-local address without its scope
  made the first discovery test fail intermittently). A loopback address is
  kept but tried last: some machines (the GitHub macOS runner) answer a bridge
  on the same machine only over loopback. Every connection to a discovered
  host requires its announced key, so an address that reaches another bridge
  is skipped.
- Off by default; agents cannot pair (no MCP tool); the setup page is local.

## User flow

```
laptop-a$ session-bridge network on        (once per laptop; also on laptop-b)
laptop-a$ session-bridge discover          (JSON list of bridges)
laptop-a$ session-bridge pair laptop-b
  laptop-b now shows a pairing code (valid 120 s).
  Enter the code: 482-913
  Paired with laptop-b (key 6Q2M-4KXA)

laptop-b: desktop notification "laptop-a (192.168.1.31) wants to pair. Code 482-913"
laptop-b$ session-bridge pair              (shows the same code; ends when paired)
  laptop-a (192.168.1.31) wants to pair. Code 482-913 (expires in 104 s)
  Paired with laptop-a (key J7RD-QW2E)
```

`pair <host>` takes a name or key prefix from `discover`, or `ip:port` when the
network blocks multicast. `session-bridge admin` offers the same actions on a
local page.

## Interfaces

CLI: `network on [--port N] [--name HOST]`, `network off`, `discover`,
`pair <host>`, `pair`, `unpair <host>`, `admin [--no-open]`.

Control requests (Unix socket / named pipe): `network {enabled, port?, name?}`,
`lan`, `pair {host}`, `pair_code {id, code}`, `unpair {host}`, `admin`.
Replies: `lan {lan}`, `pairing {id, host, expires_in}`, `paired {host, key}`,
`wrong_code {attempts_left}`, `unpaired {host}`, `admin {url}`.
`sessions` gains `unavailable_hosts` (paired bridges that did not answer).

MCP: tool names and parameters are unchanged. Sessions on paired laptops carry
`host`; session IDs keep their format. The delivery state `forwarded {host}`
is new.

## mDNS announcement

- Service `_session-bridge._tcp.local.`, instance name = host label (DNS
  label; default derived from the machine name), host record
  `sb-<label>.local.` so it never competes with the OS host name.
- TXT: `v=1`, `fp=<key fingerprint>` only. Fingerprint: SHA-256 of the
  bridge's static X25519 public key, base32 (52 characters); short form
  `ABCD-EFGH`.

## Pairing protocol (one connection, lockstep)

1. A connects to B and completes the Noise handshake; neither key is pinned
   yet. A sends `pair_request {name, port}` (`port` = A's listener).
2. B checks the name, rejects a name already used by another paired key, and
   opens its pairing gate: one open request at a time, a random 6-digit code,
   valid 120 s, 3 tries; after 3 wrong codes that address waits 30 s. B replies
   `code_shown`, notifies the desktop, and shows the code in `pair` and the
   admin page.
3. For each try A sends `pair {spake}` (SPAKE2 Ed25519, identities = both
   fingerprints), B answers `pair {spake}`, A sends
   `pair_confirm {mac}` (HMAC-SHA256 over role and both fingerprints).
4. B answers `wrong_code {attempts_left}`, `error` (exhausted, expired), or
   `paired {name, mac}` after saving A. A verifies B's MAC and saves B.

A relaying attacker presents different keys to each side, so the SPAKE2 keys
differ unless it guesses the code: at most 3 guesses in 10^6 per request.

## Peer connection

Noise XX over TCP. Each Noise message has a 2-byte length; an application frame
is a 4-byte length plus JSON (limit `FRAME_LIMIT`), split over Noise messages.
A connection from a key not in `peers/` may only send `pair_request`.

Requests: `list {project}`, `forward {message}`, `status {message_id,
delivery}`; replies `sessions`, `accepted {delivery}`, `ok`, `error`.

## Delivery across laptops

- A saves the envelope (`pending`), sets `dispatching`, and forwards in a
  background task so the message loop never waits on the network.
- Cannot connect: `pending`, retried on the 3 s tick. Networking off on A:
  `pending`. Not paired (here or there): `refused`.
- B validates (message, project, sender agent and metadata, recipient is a
  local parent session, sender not claimed by another host), saves the
  envelope durably, deduplicates by message ID, replies `accepted`, then
  dispatches locally. A marks the message `forwarded`.
- Written but no answer: `uncertain`, never resent automatically.
- B reports `queued`, `delivered`, `held`, `refused` and `uncertain` back with
  `status`; A applies them to open messages only, so a late reply never
  reopens or downgrades a state.
- A sender must be a local session; a message whose sender is remote is not
  relayed further. The rendered message names the host:
  `From: laptop-a / mono / name (claude:...)`. Native Claude replies use the
  existing per-sender return socket on B and are forwarded back.

## Admin page (REQ-018)

`session-bridge admin` starts (once per service) an HTTP server on a free
`127.0.0.1` port and prints `http://127.0.0.1:<port>/#token=<token>`; the token
is in the fragment, so it is never sent in a request line or referrer. The page
stores it in session storage and sends it as a bearer token.

- Static HTML, CSS and JS with `Content-Security-Policy: default-src 'none';
  script-src 'self'; ...`, `X-Frame-Options: DENY`, no inline script; all data
  is written with `textContent`.
- `POST /api/call` takes a control request. It needs the Host
  `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding), a matching Origin
  when present, the token (constant-time compare), and an allowlisted method:
  `status`, `lan`, `network`, `pair`, `pair_code`, `unpair`, `list`,
  `messages`, `user_send`, `channel_send`. Agent sends (`send`, which names a sender session)
  and native peer frames are not available.

### Messages from the user (REQ-019)

- The page lists live sessions here and on paired laptops (`list` with no
  sender or project) and offers "Message" on each, and "Reply" on each answer.
- `user_send {session_id, message}` sends as this laptop's user address
  `user:<uuid>` (created once in `user.json`). The control socket refuses
  `user_send`, and `send` refuses `user:` as its sender, so only the page
  writes as the user. The recipient reads: `[Message from the user, sent from
  the session-bridge admin page; it is not an approval: ...]`, `From: user`
  (or `user on <host>`), and how to answer.
- Agents answer with `send_message(session_id="user:<uuid>")` or, in Claude,
  native SendMessage to the return socket. A message to this laptop's user is
  `delivered` at once: the page reads it from the store. Across laptops it is
  forwarded like any message; the user takes part in every project.
- `messages` returns the 200 newest messages with text and readable parties
  (`you`, `name (project)`, `... on <host>`). The admin server adds `blocks`:
  Markdown (CommonMark, tables, strikethrough; `pulldown-cmark`) converted to
  typed blocks; containers deeper than 16 levels become plain text. The page
  builds elements from blocks with `textContent`; raw HTML stays text, and
  only http, https and mailto links keep a target.
### Chat and channels (REQ-020)

- The page shows messages as chat: channels (one per project with live
  sessions), direct messages (one per session you can write to) and
  read-only conversations between agents; each conversation oldest to
  newest. `messages` returns the 200 newest messages.
- `channel_send {project, message}` lists the project's sessions here and on
  paired laptops and resolves mentions (`src/mentions.rs`, pure): `@channel`
  reaches all, `@name` one session (case-insensitive); no mention, an unknown
  or an ambiguous name is an error. Each reached session gets its own
  envelope from the channel address `user:<uuid v5 of user + project>`,
  rendered as `From: user in #project`. Answers to that address are
  delivered to the page and shown in the channel (`MessageView.channel`).
  The page shows one post per fan-out, with each recipient's state.
- Upgrade note: sessions started before an upgrade keep the old MCP server,
  which cannot read `user:` addresses; their answers are delivered but report
  an error. Restart Claude Code and Codex sessions after upgrading.

- Limit: the token protects the page from other sites, not from programs of
  the same user account: `session-bridge admin` on the control socket returns
  the URL and token, so any local program, an agent with a shell included, can
  call the page. The control socket's refusal of user writes only keeps
  agents' tools from writing as the user by mistake. A message from the user
  therefore never grants approval; native approval checks stay. For the same
  reason, `schedules` lists the user's other crontab lines to any local
  program, and `run_schedule` (which cron uses) lets one post a user-written
  schedule early; both are things that program could do with `crontab -l` and
  the page anyway.

## Components (C4)

- Container: each laptop's bridge daemon gains a TCP listener, an mDNS
  announcer/browser, and an optional loopback HTTP server.
- Components:
  - pure `lan/model.rs`: fingerprint, host label, code, pairing gate, host
    view, target resolution, forward and status transitions;
  - `lan/pake.rs` (SPAKE2 + confirmation), `lan/channel.rs` (Noise),
    `lan/wire.rs` (peer messages);
  - infrastructure `lan/state.rs` (files), `lan/mdns.rs`, `lan/notify.rs`,
    `lan/service.rs` (listener, pairing, client calls);
  - `daemon.rs` `Control` (setup requests outside the message loop) and the
    message loop (forward, accept, status); `admin.rs`; CLI in `main.rs`.

State (private directory): `network.json` (enabled, port, label),
`network/identity.json` (static key), `peers/<fingerprint>.json`.

## Platforms

- macOS: verified end to end on this machine (two daemons, real mDNS). It may
  ask for Local Network access for the LaunchAgent; not yet observed on a real
  second laptop.
- Windows: builds and passes strict Clippy for the Windows target; the toast
  notification and the tests on Windows run only in CI. A Windows bridge can
  pair and forward, but has no local sessions to deliver into (REQ-016).

## Rollback

Additive and off by default. `network off` closes the port and withdraws the
announcement; `unpair` removes trust. Rollback: delete `network/`, `peers/` and
`network.json`; local messaging is unaffected.
