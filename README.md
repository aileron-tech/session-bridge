# session-bridge

A local Rust MCP bridge for existing Codex and Claude Code sessions on macOS:
agents message each other, and you talk to all of them from one chat, across
your laptops and remote hosts.

![session-bridge admin page: channels, sessions grouped by peer, roles, scheduled posts](docs/media/demo.gif)

[Watch the 45-second demo (MP4)](docs/media/demo.mp4). The demo runs three
simulated machines in Docker ([demo/](demo/README.md)) with scripted stand-in
agents.

## What it does

- **Messaging between sessions**: every Codex and Claude Code session gets
  `list_sessions` and `send_message` (MCP), and Claude's own `/list-agents`
  sees Codex sessions too.
- **One chat for you** (`session-bridge admin`): a channel per project, direct
  messages per session, conversations between agents read-only, Markdown
  formatted. `@name`, `@role` or `@channel`; a channel can have a default
  recipient. Your messages are your words, never an approval.
- **Several laptops**: bridges find each other on the local network (Zeroconf)
  and pair with a one-time code; their sessions appear in the same chat.
- **Remote hosts over SSH**: a bridge on a server is reached through an SSH
  tunnel with your own keys; no public port.
- **Roles**: named instructions you assign to sessions; the role shows as a
  badge and `@role` reaches every holder.
- **Scheduled posts**: cron schedules in your crontab (one marked line each),
  for example a status kick-off to your lead session every 10 minutes.
- **Menu-bar icon, updates**: status and controls in the macOS menu bar;
  `session-bridge update` installs a newer release after checking its
  checksum.

| Chats, grouped by peer | Channel with roles and mentions |
| --- | --- |
| ![Chats tab](docs/media/chats-peers.png) | ![Channel](docs/media/channel-reply.png) |
| **Peers: local network and SSH** | **Roles** |
| ![Peers tab](docs/media/peers.png) | ![Roles tab](docs/media/roles.png) |
| **Scheduled posts** | **Channel default recipient** |
| ![Cron tab](docs/media/cron.png) | ![Channel](docs/media/channel.png) |

## Sessions and tools

Windows runs the service, CLI, and MCP server; delivery into live Windows
sessions is not available yet (see [Windows](#windows)).
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

From a release (macOS Apple silicon shown; `x86_64-apple-darwin` for Intel,
`x86_64-pc-windows-msvc.exe` for Windows):

```sh
curl -fsSLO https://github.com/aileron-tech/session-bridge/releases/latest/download/session-bridge-aarch64-apple-darwin
curl -fsSLO https://github.com/aileron-tech/session-bridge/releases/latest/download/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
chmod +x session-bridge-aarch64-apple-darwin
./session-bridge-aarch64-apple-darwin install
```

The binaries are not code-signed or notarized; the checksum shows the file is
the one the release published. Later releases install with
`session-bridge update`.

From source:

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

## Windows

Status: partial. What works, checked by the `windows-latest` CI job:

- `session-bridge.exe install` copies the executable to
  `%USERPROFILE%\.local\bin`, registers the MCP server in both CLIs, and creates
  a per-user Scheduled Task named `session-bridge`. The task starts the service
  at logon and restarts it one minute after a failure. It needs no elevation and
  stores no password.
- The service, `list`, `status`, the MCP server, and the durable store. The
  control endpoint is a named pipe that only your user can open; a client
  refuses a pipe served by another user.

What does not work yet: discovering live Claude Code and Codex sessions and
delivering into them. On Windows `list` returns no sessions and `send` reports
an unknown recipient. Claude's Windows inbox is an authenticated named pipe and
Codex discovery needs a Windows handle scan; both need facts from a real
installation. To help, run `tools/windows-facts.ps1` with one Claude Code and
one Codex session open, review the output, and share it.

Differences from macOS: the state directory relies on the access control list
of your user profile instead of Unix modes; the Codex MCP entry is not marked
`required` until delivery works; the service log is
`%USERPROFILE%\.local\state\session-bridge\daemon.stderr.log`. A console window
may flash at logon before the service detaches.

Remove: `session-bridge.exe uninstall` (the logon task), then the two
`mcp remove` commands below.

## Run without the background service

The bridge also runs from a shell, on macOS and Windows, without the
LaunchAgent or logon task:

```sh
session-bridge uninstall      # optional: remove the background service only
session-bridge daemon         # runs in this shell until Ctrl+C
```

Run it in the background with `nohup session-bridge daemon >> bridge.log 2>&1 &`
(macOS) or `Start-Process session-bridge.exe daemon -WindowStyle Hidden`
(Windows PowerShell). Clients (CLI, MCP servers) connect to whichever bridge
runs for their state directory; only one bridge can own a state directory.

A separate bridge, for example to try a build beside the installed one, uses
its own absolute state directory: its own control socket or pipe, admin page,
pairing and messages. Clients reach it only with the same variable:

```sh
export SESSION_BRIDGE_STATE=/tmp/bridge-test
session-bridge daemon &
session-bridge status         # this bridge, not the installed one
```

With `SESSION_BRIDGE_STATE` set, a client that finds no bridge says so instead
of starting the installed service. Use `network on --port <n>` for a second
networked bridge on the same laptop. `session-bridge uninstall` removes only
the service registered for this home; the executable, MCP entries, pairing and
messages stay.

## Menu-bar icon (macOS)

The bridge started by its LaunchAgent shows an icon in the menu bar (a bridge
arch). Its menu shows the version, whether the bridge answers, how many
sessions it sees, networking, and whether it starts at login or runs from a
shell, and offers:

- **Open admin**: the admin page in your browser.
- **Install service** (bridge run from a shell) or **Uninstall service...**.
- **Check for updates**, then **Update to x.y.z...** when a newer release
  exists (see Updates).
- **Shut down...**: after confirmation, stops the bridge; the service starts
  again at your next login or with `session-bridge install`.

A bridge run from a shell shows the icon with `SESSION_BRIDGE_TRAY=1`. A
separate bridge (`SESSION_BRIDGE_STATE`) offers neither install nor uninstall,
and `install` refuses to replace a service registered for another home. The
icon appears after the next `session-bridge install`, which adds
`SESSION_BRIDGE_TRAY=1` to the LaunchAgent.

## Roles

The **Roles** tab keeps roles: a name and the instructions a session with
that role follows, for example `reviewer`: "Review every change for
correctness and risk; reply with blockers first". Assign a role to a session
and its instructions are sent to it once, as your message (never an
approval). The session then shows a role badge, and `@reviewer` in a channel
reaches every session with that role. A role can also be a channel's default
recipient.

## Start and close sessions

In a channel, **New session** starts a Claude or Codex session with a role.
It is named `{short}-{role}-{n}` (for example `mono-reviewer-2`, the lowest
free number), starts in the project's directory, and gets the role's
instructions as its first prompt. The short name defaults to the project
folder's name; change it in the same form. Where it opens:

| System | Where |
| --- | --- |
| macOS | A Terminal window, or iTerm2 (choose in the form) |
| Windows | A `cmd` console window |
| Linux | Off screen: a tmux session, else screen, else a detached `script`; attach with `tmux attach -t <name>` or `screen -r <name>` |

A started session runs the plain `claude` or `codex` command with its normal
approval prompts: no shell aliases and no permission-skipping flags. A
session's **Close session** button (local sessions only) ends its process
after you confirm. A Codex session that runs inside a shared Codex app server
is not closed this way; close it in its terminal. From a shell:
`session-bridge spawn <claude|codex> <role> [--project <name-or-path>]` and
`session-bridge close <session_id>`.

The built-in **root** role coordinates a project. A session with it gets two
more MCP tools: `spawn_session(agent, role)` starts a session with any role
but root in its own project (at most 8 live sessions there), and
`close_session(session_id)` closes a session there that does not hold root.
Only you assign root or start a root session; you can edit root's
instructions on the Roles tab but not rename or delete it. Roles are kept by
session name, so like every role, root follows a session's name; any program
of your account can also run `session-bridge spawn`, so root is a convenience
for coordinating agents, not a security boundary.

A direct conversation with a session has a **Kickoff** button that fills in
a request for its issue status and its queued work in priority order.

## Scheduled messages

The admin page's **Scheduled messages** panel posts a message in a project
channel on a cron schedule, for example every 10 minutes to `@master` in
`#mono`. Choose the recipient: one session, `@channel`, or the channel default.

- Each schedule is one line in your crontab, marked `# session-bridge
  schedule`, that runs `session-bridge scheduled <id>`. Your other crontab
  lines are shown read-only and kept exactly as they are.
- The message arrives as your post in the channel, tagged *scheduled*; it is
  your words, never an approval. If the bridge is not running at that minute,
  that run is skipped.
- "Run now" posts a schedule at once. Deleting a schedule removes its line.
- Not available on Windows (no crontab).

## Updates

Releases from 0.3.0 on carry prebuilt binaries for macOS (Apple silicon and
Intel) and Windows (x86_64), with a `SHA256SUMS` file:

```sh
session-bridge update --check   # is a newer release available?
session-bridge update           # download, verify, install, restart the service
```

The binary for this platform is downloaded with the system `curl` and
installed only if its SHA-256 matches `SHA256SUMS`. With the background
service installed, the new version installs itself (executable, service,
MCP entries) and restarts the service; otherwise it replaces
`~/.local/bin/session-bridge`. The binaries are not code-signed or notarized:
the checksum proves the file is the one the release published, not who built
it. Nothing checks for updates unless you ask. Restart running Claude Code and
Codex sessions afterwards so their MCP server uses the new version.

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
| `session-bridge uninstall` | Remove only the background service (LaunchAgent or logon task) |
| `session-bridge stop` | Stop this home's bridge (the service until next login, or one run from a shell) |
| `session-bridge remote add <ssh-host> [--port <n>]` / `remote list` / `remote remove <ssh-host>` | Reach a bridge on another host through an SSH tunnel |
| `session-bridge update [--check]` | Install (or only report) the latest release's binary, checked against its SHA256SUMS |
| `session-bridge daemon` | Run the bridge in this shell until Ctrl+C (`SESSION_BRIDGE_STATE` for a separate one) |
| `session-bridge list [--project <name-or-path>]` | List live parent sessions (default: current directory) |
| `session-bridge status` | Show the service state and recent messages |
| `session-bridge send <session_id> <message> [--project <name-or-path>]` | Send from an agent's shell tool |
| `session-bridge spawn <claude\|codex> <role> [--project <name-or-path>]` | Start a session with a role in a terminal |
| `session-bridge close <session_id>` | End a local session's process |
| `session-bridge network on [--port <n>] [--name <host>]` / `network off` | Announce this laptop on the local network and accept paired bridges |
| `session-bridge discover` | Bridges found on the network, paired or not |
| `session-bridge pair <host>` | Pair with a discovered laptop; type the code it shows |
| `session-bridge pair` | Show the code of an incoming pairing request |
| `session-bridge unpair <host>` | Forget a paired laptop |
| `session-bridge admin [--no-open]` | Open the local page: setup, sessions, messages |
| `session-bridge mcp` / `native-peer` | Internal: run by the MCP clients and the relay |

MCP tools: `list_sessions(project?)` and `send_message(session_id, message, project?)`;
for root sessions also `spawn_session(agent, role)` and `close_session(session_id)`.
Agent-facing rules for using them are in [AGENTS.md](AGENTS.md) (`CLAUDE.md` is a
symlink to it).

## Several laptops

Sessions on your other laptops on the same network can be listed and messaged
once their bridges are paired. Pairing works like adding a printer: every
bridge announces itself (Zeroconf: mDNS + DNS-SD), you pick one, it shows a
one-time code, and you type that code on the laptop you are using.

```sh
session-bridge network on              # on each laptop, once; remembered
session-bridge discover                # bridges found on the network
session-bridge pair laptop-b           # laptop-b shows a code; type it here
```

The chosen laptop shows the code as a desktop notification, in
`session-bridge pair`, and on the setup page (`session-bridge admin`). A code is
valid for 2 minutes and allows 3 tries; after that the asking address waits 30
seconds. When multicast is blocked, pair by address: `session-bridge pair
192.168.1.20:47820`.

- Networking is off until you turn it on. The bridge then listens on TCP port
  47820 on IPv4 (macOS may ask for Local Network access; Windows Firewall may
  ask to allow the port).
- The announcement contains only a protocol version and the bridge's key
  fingerprint: no project, path, user or session.
- The code is checked with SPAKE2 bound to both bridges' keys, so it cannot be
  guessed offline or relayed. Paired bridges then use Noise
  (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) with pinned keys for every connection.
- Agents cannot pair: there is no MCP tool for it.
- Remote sessions appear in `list_sessions` with a `host`. Their directories
  differ from yours, so pass the project name (`--project mono`) rather than a
  path. A paired bridge that does not answer is listed in `unavailable_hosts`.
- A message to a remote session is `forwarded` once the other bridge saved it;
  that bridge then delivers it and reports `queued`, `held`, `delivered`,
  `refused` or `uncertain` back. If the other laptop is away the message stays
  `pending` and goes out when it returns.

Design: [docs/lan-discovery.md](docs/lan-discovery.md).

## Remote hosts over SSH

A bridge on a server or a laptop elsewhere is reached through SSH, never a
public port. On the remote host:

```sh
session-bridge network on --local-only   # listens on 127.0.0.1 only, no announcement
```

Here (or on the Peers tab of the admin page):

```sh
session-bridge remote add user@server    # an ~/.ssh/config alias works too; --port if not 47820
session-bridge pair user@server          # type the code the remote bridge shows
session-bridge remote list               # tunnel state: connected, connecting, down
```

The bridge keeps one `ssh -N` connection with your keys or ssh-agent (no
password prompts), forwards a local port to the remote bridge and a port on
the remote's loopback back to this one, and reconnects when it drops. Inside
the tunnel the bridges still use their pinned, encrypted channel, and pairing
still needs the one-time code. Servers that ask for a typed one-time password
on every SSH login are not supported yet.

## Admin page

`session-bridge admin` opens a page on `127.0.0.1` (this laptop only) with
four tabs: **Chats** (talk to your sessions), **Cron** (scheduled messages),
**Peers** (networking and pairing) and **Roles**. Every session shows a badge
with its peer (the laptop it runs on); the chat sidebar groups by peer or
project, filters by peer and sorts by recent activity or name.

- **Chat**, like Slack: a channel per project (`#mono`), a direct conversation
  per session, and read-only conversations between agents. Message text is
  formatted from Markdown (headings, lists, tables, code). Raw HTML is shown as
  text; only http, https and mailto links are clickable.
- In a channel, `@channel` reaches every session of the project (on this
  laptop and paired laptops) and `@name` one session. Set a **default
  recipient** in the channel header (your lead or "master" session): a post
  without a mention goes to it; otherwise a post must mention someone.
  Answers to a post appear in the channel.
- The sidebar groups each project's channel and sessions together, and
  conversations between agents by the agent that started them.
- A message you write reaches the session as your words, marked as sent from
  the admin page. It never approves an action that needs permission; the
  session still asks you as usual.

After upgrading, restart running Claude Code and Codex sessions: their MCP
server starts with the session and keeps the old version until then.

The link lasts until the service restarts; run `session-bridge admin` again
for a new one.

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
Changes per version: [CHANGELOG.md](CHANGELOG.md).

## Verification

```sh
cargo test --lib --test control --test routing
cargo test --test messaging --test peer_auth -- --test-threads=1
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs these on macOS and Windows (`.github/workflows/ci.yml`); the routing,
messaging, and peer tests are Unix only, and a separate Windows job drives the
real installer and reads the service back.

The routing tests use real child processes and Unix sockets to check native
frames, durable pending work, held receipts, reply routing, daemon restart,
destination restart, MCP discovery, and reconnection. They do not invoke models.
Live provider checks are reported separately in LOG.md.

## Remove

Remove the `session-bridge` MCP entry with `codex mcp remove session-bridge` and
`claude mcp remove --scope user session-bridge`, then remove the background
service with `session-bridge uninstall` (it unloads the LaunchAgent and deletes
its plist). Remove the executable if desired. Native Codex helper files belong to Codex. Retain its state directory until
pending/uncertain messages have been resolved. No Git configuration is changed.
