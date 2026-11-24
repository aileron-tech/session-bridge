# Changelog

User-facing changes to session-bridge. Newest first. Each version lists
`### Changes` and then `### Fixes`.

## Unreleased

### Changes

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
