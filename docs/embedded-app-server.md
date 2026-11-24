# Embedded Codex App Server

Upstream source commit: `e58bf8f5233b51e7cd1679d947e3028c1ed91b06` from
`https://github.com/openai/codex.git`. `vendor/codex` is a pinned Git submodule,
as requested by the user on 2026-11-13. Validation results and installation state
are recorded in LOG.md.

## Scope and ownership

The bridge must communicate with the same runtime that owns the user's Codex
threads. The single-binary deployment will host Codex App Server inside the
bridge daemon and expose its normal private Unix WebSocket endpoint. Codex
terminals and the bridge's routing client connect to that endpoint.

System context: independent parent Codex and Claude sessions coordinate work.
Container: one bridge executable hosts the bridge daemon and Codex App Server.
Components: the existing durable bridge outbox, native Codex JSON-RPC client,
upstream App Server runtime, and native Claude peer adapter.
Contracts: upstream App Server JSON-RPC over Unix WebSocket; existing bridge
MCP tools and native Claude messages. Parent filtering and project filters stay
at the bridge boundary. Agent messages never represent user approval.

## Runtime entrypoints

- Link `codex-app-server` from `vendor/codex/codex-rs/app-server`.
- Start `codex_app_server::run_main_with_transport_options` with
  `AppServerTransport::UnixSocket`, the existing Codex home/configuration, and
  the same `Arg0DispatchPaths` used by the host. This creates the shared runtime
  and network listener together. The bridge uses its native client to connect.
- Do not start an additional `in_process::start` runtime: it owns a separate
  single-connection MessageProcessor and supplies no public multi-client socket
  attachment entrypoint. Two starts would create two thread managers.
- Reuse `codex_arg0::arg0_dispatch_or_else` before the bridge's ordinary command
  parsing. Preserve apply_patch, execution, filesystem and platform sandbox
  self-reexecution modes, plus upstream runtime stack sizing.
- If a server already answers at the endpoint, attach to it. Otherwise host it
  in the bridge process by default. With `--codex-app-server`, instead invoke
  the native `codex app-server daemon start` command. Failed startup is logged
  and retried; undelivered messages remain pending.
- Once ownership is acquired, a second bridge/server must not replace its
  endpoint. Use upstream private-socket startup locking.

## One binary and helper isolation

Upstream code mode launches `codex-code-mode-host` as a separate process.
`InstallContext::code_mode_host_program` searches packaged resources and then
the executable's directory. `Connection::spawn` passes no subcommand.

For one compiled executable, link `codex-code-mode-host` and its runtime too.
Install a sibling `codex-code-mode-host -> session-bridge` symlink and dispatch
that argv[0] to `codex_code_mode_host::run_main` on the helper's expected runtime.
Handle `--codex-ui-plugin-process` before ordinary command parsing; that mode
calls `codex_code_mode_runtime::run_ui_sandbox`. The plugin launcher canonicalizes
the program path, so argv[0] alone is insufficient for this second mode.

This gives one executable artifact with multiple process roles. Preserve helper
process isolation. Shell commands, MCP servers, and system tools remain external
programs as they are in Codex itself.

## Build requirements

The project selects the upstream Rust 1.95.0 toolchain. Initialize the pinned
GitHub submodule with `git submodule update --init --recursive`. Cargo follows
the runtime/helper dependency graph rather than building the entire upstream
workspace. Third-party Cargo downloads and the native V8 artifact remain build
inputs. The submodule is unmodified; bridge-specific dependency patches live in
the bridge's root Cargo manifest.

`.cargo/config.toml` selects the OpenAI `rusty-v8-v152.2.0` artifact release,
matching the submodule's `.github/actions/setup-rusty-v8/action.yml`. The default
Deno URL returns 404 for this sandbox artifact. The source-controlled hashes in
`vendor/codex/third_party/v8/rusty_v8_152_2_0.sha256` verify the archive and Rust
bindings. Update the artifact tag together with the submodule's V8 dependency.

The upstream root patch sections must be retained at the new Cargo root.
A temporary standalone Cargo metadata probe with only the App Server path
dependency failed: the registry `tokio-tungstenite` 0.28.0 lacks the required
`proxy` feature. Codex pins a fork with that feature. Its other pins include
SQLx, tungstenite, and rusty_v8. Record the source commit and preserve the
dependency lock; the local package version is 0.0.0, not a release identifier.

A full compile/link and runtime helper test are required before claiming a
self-contained embedded build. Dependency resolution is not that proof.

## Lifecycle and migration

The bridge and hosted server share a process lifetime. The entrypoint must keep
the bridge serving MCP and peer messages while App Server drains active turns,
then clean up relay processes. Respect AppServerExit instead of aborting the
server task. Evaluate upstream managed-daemon snapshot/recovery support without
giving the separate Codex updater ownership of the bridge executable.

Existing embedded terminal sessions require an idle exit and
`codex --remote unix:// resume <thread-id>`. Never load their history into a
second runtime while the original is running. Keep pending peer messages until
their target joins the shared server. No transcript is broadcast to peers.

## Acceptance

1. Build a single executable against the pinned source and upstream patches.
2. Launch with an isolated Codex home, with and without --codex-app-server.
3. Attach two terminal clients to the same hosted runtime. Verify parent names,
   projects, subagent exclusion, native queue receipts and live status events.
4. Exercise shell execution, apply_patch, code mode and plugin helper dispatch.
5. Verify Codex-to-Claude and Claude-to-Codex messages with the same tool schema.
6. Restart the owner process and prove recovery of pending messages, reconnect,
   thread restoration and MCP availability; verify graceful shutdown during work.
