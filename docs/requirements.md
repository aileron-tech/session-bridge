# Requirements

- REQ-001: Discover live local Codex and Claude sessions with project and identity.
- REQ-002: Route messages and replies to the specified existing session.
- REQ-003: Preserve pending work and resume routing after service/dependency restarts.
- REQ-004: Preserve native approvals and report held/refused/uncertain delivery honestly.
- REQ-005: Automatically load the bridge for Codex and Claude at startup.
- REQ-006: Validate messages, identities, filesystem and local socket boundaries.
- REQ-007: Expose and message parent sessions only; subagents remain managed by their parents.
- REQ-008: Filter session discovery and recipient delivery by an optional exact project name or absolute directory path, including pending retries.
- REQ-009: Advertise parent Codex sessions as native Claude-compatible peers with accurate process identity and route native messages to their existing Codex queues.
- REQ-010: Connect persistently to the local Codex App Server, discover loaded parent threads, consume live status events, reload MCP, and route durable messages without launching duplicate sessions; reconnect after dependency restart.
- REQ-011: Reuse a running Codex App Server. When absent, host it inside the bridge by default; --codex-app-server instead starts the external native daemon. Persist the selected mode when installing the background bridge.
- REQ-012: Reference Codex through a pinned GitHub submodule under vendor/codex; link only required runtime/helper modules and their dependencies, without paths to an external source checkout.
- REQ-013: A single bridge executable also dispatches the code-mode, plugin, execution and patch helper roles required by embedded Codex.
- REQ-014: MCP listing and sending default to the caller's exact working directory; CLI listing defaults to its current directory. Other projects require an explicit project filter. Persist the resolved send scope for retries.

- REQ-015: The bridge provides messaging only. It never embeds, starts, configures, or connects to Codex App Server; model catalogs and runtimes remain owned by native clients. Discover live sessions from process-owned metadata and deliver through native `codex queue` without `--resume` or model overrides. The native CLI owns any runtime connection. Preserve durable delivery semantics. Supersedes the App Server and helper requirements REQ-010 through REQ-013.
