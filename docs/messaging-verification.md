# Messaging-only verification (2026-11-20)

The bridge no longer links Codex or V8, hosts an App Server, connects to its RPC,
changes model catalogs, or dispatches native helper roles. Codex delivery calls
`codex queue --thread <id> --message <text>` with the existing Codex home and no
resume, model, or approval overrides. The native CLI owns its runtime connection.

## Evidence

- Baseline isolated old-binary test failed: startup reported hosting=true and
  "Starting embedded Codex App Server" even without an available Codex executable.
- Native backend catalog comparison returned four visible models for version
  0.0.0 and ten for the installed native CLI. Omitting client_version returned 400.
- Native 0.171.0 isolated model/list returned all ten visible models, including
  gpt-6.2-sol, gpt-6.2-astra and gpt-6.1-terra. Native queue accepted the isolated
  test thread message. See messaging-native-verification.json.
- Final affected coverage: 6 library tests, 5 messaging tests, 10 routing tests,
  and 1 peer authentication test passed. Routing covers persisted pending/held
  work across daemon restart, destination recovery, scope, MCP reconnect and peers.
- A new discovery test first failed on macOS /var versus /private/var aliases.
  Canonicalizing the discovery root fixed the implementation; the assertion was
  retained. Attached thread metadata remains classified as subagent.
- cargo fmt --check passed.
- cargo clippy --workspace --all-targets --offline -- -D warnings passed.
- cargo tree contains no codex-* or v8 dependency. Debug executable is about 15 MB.

## Graph commands and coverage

Wingbox quick embedded; analyze src/app_server, src/daemon, src/install,
src/main, src/mcp, src/discovery, src/transport, src/codex_sessions;
impact on all changed source/test paths before their first edit; build .
--exclude-dir vendor; usages queue; usages discover; analyze tests/messaging.
The original impact outputs are retained beside this report. Queries on manifests
and Markdown report no-match because those are not source modules. The obsolete
vendor graph build was stopped after the user requested a messaging-only bridge.
Cross-repository native protocol and string-key checks used literal text search.

| Changed file | Named dependents and coverage |
| --- | --- |
| src/codex_sessions.rs | daemon, lib, client, install, main, mcp, native_peers: queue argument/failure tests, native CLI probe, routing/MCP/peer tests; install checked by source review and existing configuration reuse during deployment |
| src/discovery.rs | daemon, codex_sessions, main, mcp, native_peers, protocol, transport, lib, routing: writer-handle/closed-handle and attached-thread tests, parent/project/MCP/peer routing tests |
| src/daemon.rs | client, install, main, mcp, native_peers, lib: startup without Codex, shutdown, all ten routing and recovery tests |
| src/protocol.rs | daemon, client, main, mcp, native_peers, transport consumers: status serialization/startup and routing tests |
| src/main.rs | no graph dependents: process-level messaging and routing tests |
| src/install.rs | main, lib: compilation and source review of removal of server flag/helper ownership; full installer not rerun because existing registration is preserved |
| src/lib.rs | composition boundary: compiled by all affected tests |
| removed src/app_server.rs and src/embedded.rs | their runtime consumers migrated above; no native runtime dependencies remain |
| removed tests/app_server.rs, tests/embedded.rs, tests/helpers.rs | tests of the explicitly removed hosting/helper contract superseded by REQ-015 messaging tests; native queue independently exercised |
| tests/messaging.rs | imports real queue, discovery, protocol and transport modules; only executable boundary is simulated in argument/failure cases |

Graph usages reaches queue and discover from tests/messaging.rs. Daemon/main/MCP
are reached through real child processes, which the graph cannot trace as direct
function calls. Assertions were also read. git diff_functions HEAD cannot run:
the repository has no HEAD commit, and its original source is untracked. There
is no configured remote; no unrelated pre-existing source was committed.

## Runtime migration

The installed old bridge currently hosts live Codex threads. Deployment backs up
its binary, replaces it atomically, and restarts its LaunchAgent at the user's
request. A one-time native Codex startup restores the native runtime endpoint;
this is an operator migration, not behavior in the bridge. Peer outbox files are
preserved. Final live readback is written to messaging-restart-verification.json.
