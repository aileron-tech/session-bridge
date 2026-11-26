# Changelog

User-facing changes to session-bridge. Newest first. Each version lists
`### Changes` and then `### Fixes`.

## Unreleased

### Changes

- Start a Claude or Codex session with a role from a channel's **New
  session** button or `session-bridge spawn`: it opens in a Terminal or
  iTerm2 window (macOS), a `cmd` window (Windows) or off screen in tmux,
  screen or `script` (Linux), named `{short}-{role}-{n}`, with the role's
  instructions as its first prompt and its normal approval prompts.
- Close a session on this laptop from its conversation or with
  `session-bridge close`.
- The built-in **root** role: a root session gets the MCP tools
  `spawn_session` and `close_session` to start and close non-root sessions
  in its own project.
- A **Kickoff** button in a session's conversation fills in a request for
  its issue status and queued work in priority order.

### Fixes

- Running `session-bridge network on` while networking was already on could
  leave the bridge reporting networking on but no longer listening, so
  paired laptops went offline. The old listener is now closed before the new
  one starts, and an unchanged `network on` keeps the running one.

## 0.3.0 - 2026-11-26

### Changes

- README: a feature overview with screenshots and a 45-second demo video,
  recorded from `demo/`: three simulated machines in Docker (two laptops on
  one network, one remote host reached by SSH) with scripted stand-in agents.
- Remote hosts over SSH: `session-bridge remote add <ssh-host>` keeps an SSH
  tunnel (your keys or agent) to a bridge running with `network on
  --local-only` on that host; pair it with its one-time code and its sessions
  appear like a paired laptop's. Also on the admin page's Peers tab.
- Admin page tabs: Chats, Cron, Peers and Roles. Roles are named instructions
  you assign to sessions: assigning sends the instructions once, the session
  shows a role badge, and `@role` in a channel reaches every holder. Every
  session shows its peer as a badge; the sidebar groups by peer or project,
  filters by peer and sorts by activity or name. New icon: a box with Wi-Fi
  waves, in the menu bar and on the page.
- Admin page (`session-bridge admin`) as a chat, like Slack: a channel per
  project and a direct conversation per session (this laptop and paired
  laptops), plus read-only conversations between agents. Message text is
  formatted from Markdown. A message you send reaches the session as your
  words and never counts as approval; answers appear on the page.
- Channels: `@channel` reaches every session of the project and `@name` one
  session; a channel can have a default recipient (for example your lead
  session) for posts without a mention. The sidebar groups channels and
  sessions by project, and conversations between agents by who started them.
- Scheduled messages: the admin page adds, edits, runs and deletes channel
  posts on a cron schedule (for example every 10 minutes to `@master` in
  `#mono`), each as one marked line in your crontab; your other crontab lines
  are shown read-only and left unchanged. Not on Windows.
- macOS menu-bar icon for the bridge started at login: status, Open admin,
  Install/Uninstall service, Check for updates / Update, and Shut down with
  confirmation. It appears after reinstalling (`session-bridge install`).
- Releases carry prebuilt binaries for macOS (Apple silicon, Intel) and
  Windows with SHA256SUMS. New `session-bridge update [--check]` installs the
  latest release after checking its checksum and restarts the service.
- Run the bridge without the background service: `session-bridge daemon`
  from a shell (macOS and Windows), `SESSION_BRIDGE_STATE=<absolute dir>` for
  a separate one, `session-bridge stop`, and `session-bridge uninstall`
  (removes only the background service). `install` refuses to replace a
  service registered for another home.
- After upgrading, restart running Claude Code and Codex sessions so their
  MCP server can read the new `user:` addresses.

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
