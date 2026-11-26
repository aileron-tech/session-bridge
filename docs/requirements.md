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

- REQ-016: Run the messaging service, CLI, MCP server and durable store on Windows as well as macOS. The control endpoint is a named pipe that only the current user can open, and a client rejects a pipe served by another user. Installation registers a per-user logon task that starts the service at logon and restarts it after failure, without elevation. Native session discovery and delivery on Windows are not part of this requirement.

- REQ-017: When the user turns networking on, the bridge announces itself and finds other bridges on the local network with Zeroconf (mDNS + DNS-SD, IPv4). Two bridges exchange sessions and messages only after the user pairs them: the user picks a discovered laptop, that laptop shows a one-time 6-digit code (desktop notification, `session-bridge pair`, admin page), and the user types it on the first laptop. The code is valid 2 minutes, allows 3 tries, and is verified with SPAKE2 bound to both bridges' keys; paired bridges then authenticate each other with pinned Noise keys on every connection. Networking is off by default, announcements carry only a protocol version and key fingerprint, and agents cannot pair. Sessions on a paired laptop appear in listings with their host; a message to one is forwarded to that laptop's bridge, which owns its delivery and reports its state back. Durable delivery rules apply across laptops: an interrupted forward is uncertain and never resent automatically.

- REQ-018: `session-bridge admin` opens a local setup page served by the bridge on 127.0.0.1 only. It shows this bridge's network state, discovered and paired laptops, incoming pairing codes and recent deliveries, and offers network on/off, pair, enter code and unpair. Every API call needs the per-service token from the command's URL and the page's own Host and Origin. Since REQ-019 the page also lists sessions and carries the user's messages; agent sends and native peer frames are not available there.

- REQ-019: On the admin page the user lists live sessions on this laptop and on paired laptops, writes to a session, and reads recent messages with their text formatted from Markdown. Message text is converted to typed blocks and rendered as text only: raw HTML is never markup and only http, https and mailto links keep a target. A message from the user carries a per-laptop `user:<uuid>` address, says it was written by the user through the admin page, and never counts as approval for an action that needs permission; native approval checks stay. Agents answer that address with `send_message`; the answer is `delivered` to the page, also across paired laptops. Only the admin page writes as the user: the control socket refuses it and no agent send may name the user as its sender.
