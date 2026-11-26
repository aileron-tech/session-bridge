# Changelog

User-facing changes to session-bridge. Newest first. Each version lists
`### Changes` and then `### Fixes`.

## Unreleased

### Changes

- Releases carry prebuilt binaries for macOS and Windows with SHA256SUMS.
  New `session-bridge update [--check]`, also in the menu-bar icon: installs
  the latest release after checking its checksum and restarts the service.

- macOS menu-bar icon for the bridge started at login: status (sessions,
  networking, service or shell), Open admin, Install/Uninstall service, and
  Shut down with confirmation. New `session-bridge stop`. `install` now
  refuses to replace a service registered for another home.

- Run the bridge without the background service: `session-bridge daemon` runs
  it from a shell (macOS and Windows) until Ctrl+C, and
  `SESSION_BRIDGE_STATE=<absolute dir>` runs a separate one beside the
  installed service. New `session-bridge uninstall` removes only the
  background service (LaunchAgent or logon task) and keeps messages, pairing
  and MCP entries.

- Admin page (`session-bridge admin`): lists live sessions on this laptop and
  on paired laptops, sends your messages to them, and shows recent messages
  with their text formatted from Markdown. A message you send reaches the
  session as your words and never counts as approval; the session's answer
  (`send_message` to the `user:` address in your message) appears on the page.
- Admin page chat: messages are shown as conversations, like Slack: a channel
  per project, direct messages per session, and conversations between agents.
  In a channel, `@channel` reaches every session of the project and `@name`
  one session; answers appear in the channel. Restart running Claude Code and
  Codex sessions after upgrading so their MCP server can answer you.

### Fixes

- Session listing no longer fails now and then with "Codex metadata query
  failed: unable to open database file": when Codex has checkpointed its
  database (no -wal file), the bridge reads it as an immutable snapshot.

## 0.2.0 - 2026-11-26

### Changes

- Several laptops: `session-bridge network on` announces the bridge on the
  local network (Zeroconf), `session-bridge discover` lists the bridges found,
  and `session-bridge pair <host>` pairs two laptops with a one-time code shown
  on the chosen laptop. Sessions on paired laptops appear in `list_sessions`
  with a `host` and can be messaged; the new delivery state `forwarded` means
  the other laptop's bridge saved the message. Networking is off by default.
- `session-bridge admin` opens a local setup page for networking, pairing and
  recent deliveries.
- Windows: the service, CLI, MCP server, and durable store now run on Windows.
  `session-bridge.exe install` registers a per-user logon task named
  `session-bridge` that starts the service at logon and restarts it after a
  failure, without elevation. Discovering live sessions and delivering into
  them is not available on Windows yet.
- macOS: the LaunchAgent label is now `com.aileron-tech.session-bridge`.
  Reinstalling retires an agent registered as `fr.aileron.session-bridge`;
  state, executable path, and MCP entries are unchanged.
- Added a command reference to the README and a usage reference for agents to
  `AGENTS.md`.

## 0.1.0 - 2026-11-24

### Changes

- First public release: MCP tools `list_sessions` and `send_message` for
  existing Codex and Claude Code sessions on macOS.
- Listing and sending default to the caller's exact project directory; an
  explicit `project` enables cross-project coordination.
- Codex delivery through the native `codex queue`; Claude delivery through its
  native inbox. Claude's `/list-agents` and `SendMessage` see parent Codex
  sessions.
- Pending messages survive service restarts; a message whose delivery is
  uncertain is never resent automatically.
- `session-bridge install` sets up the executable, LaunchAgent, and MCP entries.
