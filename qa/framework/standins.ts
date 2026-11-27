// Stand-in programs for a test bridge: no real agent, terminal, crontab or
// ssh is ever started. Each records what it got, for the checks.

/** Claude: `claude [flags] -n <name> <prompt>`. Registers a session record,
 * serves a private Unix-socket inbox, confirms each delivery ("delivered"),
 * and appends every received message to received/<name>.log. */
export const CLAUDE = `#!/usr/bin/env python3
import json, os, socket, sys, uuid
args = sys.argv[1:]
name = args[args.index("-n") + 1] if "-n" in args else "claude-" + str(os.getpid())
prompt = args[-1] if args else ""
home = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.join(os.environ["HOME"], ".claude")
qa = os.environ.get("SB_QA_DIR", home)
os.makedirs(os.path.join(qa, "received"), exist_ok=True)
with open(os.path.join(qa, "received", name + ".args"), "w") as f:
    json.dump(args[: args.index("-n")] if "-n" in args else args, f)
with open(os.path.join(qa, "received", name + ".log"), "a") as f:
    f.write(json.dumps({"first_prompt": prompt}) + "\\n")
inbox_dir = os.path.join(home, "i")
os.makedirs(inbox_dir, mode=0o700, exist_ok=True)
inbox = os.path.join(inbox_dir, name + ".sock")
if os.path.exists(inbox):
    os.unlink(inbox)
server = socket.socket(socket.AF_UNIX)
server.bind(inbox)
os.chmod(inbox, 0o600)
server.listen(8)
# SB_QA_MUTE: the inbox exists but refuses connections, so deliveries wait (pending).
mute = os.environ.get("SB_QA_MUTE") == "1"
if mute:
    server.close()
record = {"pid": os.getpid(), "sessionId": os.environ.get("SB_QA_SESSION_ID") or str(uuid.uuid4()),
          "cwd": os.getcwd(), "messagingSocketPath": inbox, "name": name, "status": "idle", "kind": "interactive"}
with open(os.path.join(home, "sessions", f"{os.getpid()}.json"), "w") as f:
    json.dump(record, f)
while mute:
    import time
    time.sleep(1)
while True:
    conn, _ = server.accept()
    with conn:
        line = conn.makefile().readline()
    try:
        frame = json.loads(line)
    except ValueError:
        continue
    if frame.get("type") != "user":
        continue
    with open(os.path.join(qa, "received", name + ".log"), "a") as f:
        f.write(json.dumps({"message": frame["message"]["content"]}) + "\\n")
    back = frame.get("from", "")[len("uds:"):]
    if back:
        try:
            out = socket.socket(socket.AF_UNIX)
            out.settimeout(5)
            out.connect(back)
            out.sendall((json.dumps({"type": "control", "action": "peer_message_status", "from": "uds:" + inbox,
                                     "orig_msg_id": frame.get("msg_id"), "status": "delivered"}) + "\\n").encode())
            out.close()
        except OSError:
            pass
`;

/** Codex: \`codex <prompt>\` holds its rollout file open like Codex;
 * \`codex queue --thread T --message M\` is accepted and logged. */
export const CODEX = `#!/usr/bin/env python3
import json, os, sys, time, uuid
home = os.environ.get("CODEX_HOME") or os.path.join(os.environ["HOME"], ".codex")
qa = os.environ.get("SB_QA_DIR", home)
os.makedirs(os.path.join(qa, "received"), exist_ok=True)
if len(sys.argv) > 1 and sys.argv[1] == "queue":
    args = sys.argv[2:]
    thread = args[args.index("--thread") + 1]
    with open(os.path.join(qa, "received", "codex-" + thread + ".log"), "a") as f:
        f.write(json.dumps({"message": args[args.index("--message") + 1]}) + "\\n")
    sys.exit(0)
thread = os.environ.get("SB_QA_THREAD") or str(uuid.uuid4())
name = os.environ.get("SB_QA_NAME")
os.makedirs(os.path.join(home, "sessions"), exist_ok=True)
with open(os.path.join(qa, "received", "codex-" + thread + ".log"), "a") as f:
    f.write(json.dumps({"first_prompt": sys.argv[-1] if len(sys.argv) > 1 else ""}) + "\\n")
if name:
    with open(os.path.join(home, "session_index.jsonl"), "a") as f:
        f.write(json.dumps({"id": thread, "thread_name": name}) + "\\n")
rollout = open(os.path.join(home, "sessions", f"rollout-{os.getpid()}.jsonl"), "w")
rollout.write(json.dumps({"type": "session_meta", "payload": {"id": thread, "cwd": os.getcwd(), "source": "cli"}}) + "\\n")
rollout.flush()
while True:
    time.sleep(1)
`;

/** Terminal: \`<title> <exe> launch <token> [state]\` runs in the background. */
export const TERMINAL = `#!/bin/sh
title=$1; shift
"$@" >"$SB_QA_DIR/term-$title.log" 2>&1 </dev/null &
`;

/** crontab: -l prints the table, - replaces it (a file in the test folder). */
export const CRONTAB = `#!/bin/sh
table="$SB_QA_DIR/crontab.txt"
case "$1" in
  -l) [ -f "$table" ] && cat "$table" || { echo "no crontab for qa" >&2; exit 1; } ;;
  -) cat >"$table" ;;
  *) exit 2 ;;
esac
`;

/** ssh: the forwards of \`ssh -N -L l:127.0.0.1:r -R b:127.0.0.1:o ... host\` on one machine (tests/remote.rs). */
export const SSH = `#!/usr/bin/env python3
import socket, sys, threading
args = sys.argv[1:]
forwards = []
for flag, spec in zip(args, args[1:]):
    if flag in ("-L", "-R"):
        _, listen, _, target = spec.split(":")
        forwards.append((int(listen), int(target)))
def pipe(a, b):
    try:
        while True:
            data = a.recv(65536)
            if not data:
                break
            b.sendall(data)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
def serve(listen, target):
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", listen))
    server.listen()
    while True:
        client, _ = server.accept()
        upstream = socket.create_connection(("127.0.0.1", target))
        threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
        threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()
threads = [threading.Thread(target=serve, args=f, daemon=True) for f in forwards]
for t in threads:
    t.start()
for t in threads:
    t.join()
`;

/** zsh for "Import from my shell": prints the aliases a user might have. */
export const ZSHRC_ALIASES = `alias claude='claude --model opus'\nalias codex='codex --search'\n`;
