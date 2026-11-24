# Local cross-session communication

The user runs independent Codex and Claude sessions across projects. They need
to exchange findings and coordinate concurrent edits without copying messages.

System context: the user's local CLI sessions communicate through a per-user
bridge. Container boundaries: MCP stdio clients, one supervised Rust daemon,
Claude's existing inbox sockets, and the installed Codex queue command.
Component boundaries: pure domain types and transitions; discovery, durable
outbox, and transport adapters; daemon and MCP entrypoints. Dependencies point
from adapters to domain. Cross-boundary data uses explicit serialized DTOs.

MCP tools: list_sessions(project?) and send_message(session_id, message, project?). The sender
comes from Codex's verified threadId MCP metadata, or Claude's ancestor process
session record. Missing identity is an error, not an anonymous fallback.
Without a project parameter, MCP resolves the caller's exact working directory
from registered session metadata, not the MCP process directory. CLI listing
uses its current directory. An explicit project parameter selects another
exact name or absolute path; sends remain single-recipient. Discovery and
receipt lists respect that scope. An unknown default scope is an error.
The resolved filter is saved with pending work and checked on retries. These MCP/CLI defaults are not an access-control boundary.
Only parent sessions participate. Known subagent metadata remains internal so
cached IDs and old pending messages can be explicitly refused.

Each outgoing message is durably saved before dispatch. Pending messages retry
when the destination returns. A dispatch interrupted by a crash is uncertain,
never automatically retried because native queues do not promise idempotency.
Claude held/refused results remain under Claude's approval policy. Successful
socket writes and Codex queue success mean queued, not read by the model.

Claude's directory also receives one marked native peer record per live parent
Codex session. A supervised Rust relay owns each record's actual PID and socket,
including the actual process-start token. Names identify Codex and the project;
no permission-mode attestation or Claude authentication keys are copied.
The relay forwards native frames through the daemon's typed NativePeer request.
The daemon resolves the sending Claude socket, validates the parent recipient,
and persists the message before queueing it. Marked relay records are excluded
from bridge discovery to avoid duplicates. Relay processes stop on parent death
and are recreated by the daemon; project or owning-process changes replace them.

Claude replies arrive through those native peers or previously persisted
return sockets. MCP exposes identical tools
to both agents. Peer text is explicitly identified and cannot approve actions.

Startup: user-level MCP registration starts an adapter for each client. macOS
launchd keeps the daemon running and restarts it on failure; adapters reconnect
on each operation. Discovery includes sessions opened without the MCP loaded.
The bridge has no App Server client, embedded runtime, model catalog, or helper
dispatch. Codex discovery follows this user's open rollout and writer-lock
handles, then reads thread metadata from the native SQLite database in read-only
mode. It never edits that database. Claude discovery uses its native records.
Codex delivery executes the installed `codex queue --thread <id> --message <text>`
without shell interpolation, `--resume`, model selection, or approval overrides.
The native CLI manages its own runtime connection. Failure to spawn remains
pending. Once spawned, timeout, observation failure, or nonzero exit is uncertain
and is never automatically retried. Success means queued, not read.
No conversation transcript is shared; discovery reads only session metadata.

Migration: project fields remain optional in requests and stored envelopes.
New sends and caller-identified lists without that field now use the caller's
directory. Existing queued records retain their saved scope; unfiltered historical
records are not rewritten. Explicit project filters retain their behavior. MCP
clients reload for updated tool descriptions; the daemon enforces defaults for
old clients as well. Old subagent pending
messages become refused. Native directory entries refresh without an MCP reload.
Rollback removes the MCP registrations and unloads the LaunchAgent; relay
records and sockets are cleaned up, while durable messages stay on disk.


Messaging-only migration (REQ-015): replace the bridge executable and restart its
LaunchAgent. The old embedded runtime must stop before native Codex owns its
socket again; existing terminals reconnect to native Codex or restart/resume
when idle. Keep outbox and receipt files unchanged. Native Codex owns its helper
executables, configuration, models and updates. Historical embedded build notes
are retained in docs/embedded-app-server.md; they no longer describe the build.
