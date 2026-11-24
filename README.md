# session-bridge

A local Rust MCP bridge for existing Codex and Claude Code sessions on macOS.
Every new Codex or Claude session loads the same two tools automatically:

- `list_sessions(project?)` shows live parent sessions, agent, project, working directory,
  task name, native status, and the caller's recent delivery receipts.
- `send_message(session_id, message, project?)` sends a finding, question, or reply.
  The sender comes from the calling session; it is not a tool parameter.

Subagents stay under their parents' control: they are hidden and cannot send or
receive through this bridge. Listing and sending default to the caller's exact
working directory, derived from session metadata. CLI listing uses the shell's
current directory. Set `project` explicitly to coordinate with another project;
it matches an exact project name or absolute working-directory path. A send targets one session and
rejects a project mismatch before saving or delivering. Use the full path when
different directories have the same project name. Pending retries retain this
filter, including an automatically resolved default. An unknown caller project
produces an error instead of listing all projects. This is a messaging scope, not process or security isolation. Claude's native
directory listing is separate from these MCP/CLI defaults.

Claude's native `/list-agents` also sees parent Codex sessions as compatible
peers named `codex-<project>-<short-id>`. Native `SendMessage` routes to the
existing Codex queue. One small Rust relay process per parent Codex session owns
its advertised socket and directory record; it does not run a model. These
records disappear when the session or bridge stops and are rebuilt after a
restart. Claude's permission policy still applies to messages sent into Claude.

Messages go to existing sessions. Codex delivery calls `codex queue --thread <id> --message <text>` without
`--resume`, model overrides, or permission overrides. The CLI owns any native
server connection it needs; the bridge does not embed, start, configure, or
connect directly to App Server. Claude delivery uses its native Unix inbox.

## Install

```sh
cargo build --locked --release
./target/release/session-bridge install
```

Rust 1.95.0 is selected by `rust-toolchain.toml`. The build contains only the
messaging service and adapters. The historical `vendor/codex` source is not a
build dependency. Models, authentication, and model selection belong to the
installed native clients.

The installer backs up the affected configuration, installs the executable to
`~/.local/bin/session-bridge`, registers a user LaunchAgent, and registers a
user-level MCP server in both CLIs. Codex marks this MCP server `required` so a
new session reports a startup error if the connection cannot be established.
Launchd starts the queue service at login and restarts it after failure.
Adapters connect at startup and reconnect on subsequent operations.

No shell alias, manual subscription, startup hook, or extra model is needed.
Restart a native client to load changed MCP tools. No shared runtime or
`--remote` shell alias is required by the bridge.
Claude may hold an inbound message for its user's approval. Installation does
not change `crossSessionInbound` or impersonate the sender's permission mode.

For unattended inbound delivery, Claude supports `"crossSessionInbound":
"accept"` in its user settings. This was enabled on this Mac at the user's
request and verified with a live reply on 2026-11-13. Normal tool permissions
still apply; managed or repository restrictions may take precedence. See
[Claude's inbound controls](https://code.claude.com/docs/en/cross-session-messaging#control-inbound-messages).

## Use

Ask either agent to list the local sessions or coordinate work with a named
project/session. Inspect the bridge from a terminal with:

```sh
session-bridge list
session-bridge list --project mono
session-bridge status
```

An existing agent can also send from its shell tool before reloading MCP:

```sh
session-bridge send 'codex:<session-uuid>' 'Tests are finished; you can deploy.'
session-bridge send 'codex:<session-uuid>' 'Which files are you changing?' --project mono
```

The CLI derives sender identity from `CODEX_THREAD_ID`,
`CLAUDE_CODE_SESSION_ID`, or its Claude ancestor. A human shell without an agent
identity can list/status but cannot silently impersonate a sender.

### Command reference

| Command | Purpose |
| --- | --- |
| `session-bridge install` | Install the executable, LaunchAgent, and MCP entries |
| `session-bridge list [--project <name-or-path>]` | List live parent sessions (default: current directory) |
| `session-bridge status` | Show the service state and recent messages |
| `session-bridge send <session_id> <message> [--project <name-or-path>]` | Send from an agent's shell tool |
| `session-bridge daemon` / `mcp` / `native-peer` | Internal: run by launchd, the MCP clients, and the relay |

MCP tools: `list_sessions(project?)` and `send_message(session_id, message, project?)`.
Agent-facing rules for using them are in [AGENTS.md](AGENTS.md) (`CLAUDE.md` is a
symlink to it).

## Delivery and recovery

`pending` is durably saved and retries when its destination returns. `queued`
means the native inbox accepted the write; it does not prove the model read it.
`delivered` is a native Claude receipt. `held` requires the recipient's approval.
`refused` is never retried automatically. `uncertain` means a dispatch may have
succeeded before a failure; inspect the destination before manually resending.
The bridge does not claim exactly-once delivery across the native queue boundary.

State and receipts live in `~/.local/state/session-bridge/`, owner-only. Native
Codex peer sockets live in `/tmp/session-bridge-<uid>/` and have registered
records in the configured Claude home's `sessions/` directory. Socket addresses
change when a relay restarts; Claude resolves current names through its directory.
Legacy return routes remain available for existing conversations.
Pending messages and session identities survive bridge restarts. Discovery uses
metadata for processes owned by this user and the configured Codex/Claude homes.
Codex discovery reads process-owned rollout or writer-lock handles and opens
the native metadata database read-only. It does not share conversation histories. Codex status `open` means loaded in a
live process, not necessarily actively generating a response.

Logs: `~/.local/state/session-bridge/daemon.stderr.log`.
Configuration backups: `~/.local/state/session-bridge/install-backups/`.
Source references and boundaries: [docs/design.md](docs/design.md).
Requirements: [docs/requirements.md](docs/requirements.md).

## Verification

```sh
cargo test --lib --test routing
cargo test --test messaging --test peer_auth -- --test-threads=1
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

The routing tests use real child processes and Unix sockets to check native
frames, durable pending work, held receipts, reply routing, daemon restart,
destination restart, MCP discovery, and reconnection. They do not invoke models.
Live provider checks are reported separately in LOG.md.

## Remove

Remove the `session-bridge` MCP entry with `codex mcp remove session-bridge` and
`claude mcp remove --scope user session-bridge`, then unload the LaunchAgent with
`launchctl bootout gui/$(id -u)/fr.aileron.session-bridge`. Remove only this tool's
LaunchAgent plist, executable if desired. Native Codex helper files belong to Codex. Retain its state directory until
pending/uncertain messages have been resolved. No Git configuration is changed.
