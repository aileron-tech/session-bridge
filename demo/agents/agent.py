#!/usr/bin/env python3
"""Scripted stand-in agent for the session-bridge demo (python3 stdlib only).

--kind claude: a Claude-like session. It writes ~/.claude/sessions/<pid>.json
  (pid, sessionId, cwd, messagingSocketPath, name, status, kind) and serves a
  Unix-socket inbox that takes one JSON frame per connection (PeerFrame in
  src/transport.rs). It answers through the frame's `from` return socket:
  first a `peer_message_status` "delivered" control frame, then a reply.
--kind codex: a Codex-like session. It keeps its rollout file
  ~/.codex/sessions/.../rollout-<id>.jsonl open (the bridge finds it with
  lsof, src/discovery/native.rs), names itself in session_index.jsonl, reads
  what the stand-in `codex queue` (demo/bin/codex) drops in its inbox folder,
  and answers with `session-bridge send`.

The dialogue is fixed and anonymized: a small team ships "checkout v2" of a
web shop. Replies depend on who wrote and what the message says. An agent
answers another agent only when asked a question (or in the one scripted
follow-up), so conversations always end.
"""
import argparse
import json
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid

HOST = os.environ.get("DEMO_HOST") or socket.gethostname()
HOME = os.path.expanduser("~")
DELAY = float(os.environ.get("DEMO_REPLY_DELAY", "2"))


def log(name, text):
    print(f"[{HOST}/{name}] {text}", flush=True)


# --- Incoming messages -------------------------------------------------------


def parse(text):
    """The parts of a message the bridge rendered (src/transport.rs)."""
    lines = text.splitlines()
    first = lines[0] if lines else ""
    if first.startswith("[Scheduled message from the"):
        kind = "scheduled"
    elif first.startswith("[Message from the"):
        kind = "user"
    else:
        kind = "agent"
    found = {"kind": kind, "content": "", "reply_to": None, "sender": "", "project": None}
    for line in lines:
        if line.startswith("From: "):
            found["from"] = line[len("From: "):]
        elif line.startswith("Content (JSON string): "):
            found["content"] = json.loads(line[len("Content (JSON string): "):])
    match = re.search(r'session_id="([^"]+)"', text)
    if match:
        found["reply_to"] = match.group(1)
    if kind == "agent":
        # "[host / ]project / name (id)"
        parts = found.get("from", "").split(" / ")
        if len(parts) >= 2:
            found["project"] = parts[-2]
            found["sender"] = re.sub(r" \([^)]*\)$", "", parts[-1])
    return found


# --- The script ----------------------------------------------------------------

STANDUP = """**Standup: checkout v2**

| Area | Owner | Status |
|---|---|---|
| Cart totals API | @backend | done |
| Cart page | @frontend | in review |
| Rounding fix, PR #42 | @reviewer | reviewing |
| Release notes | @docs | draft |

Next: release candidate at 15:00 if the review passes."""

PLAN = """Plan for the **checkout v2** release:

- [x] Cart totals API merged
- [ ] Review of PR #42 (rounding): asked @reviewer
- [ ] Cart page check: asked @frontend on laptop-b
- [ ] Release notes: @docs

I will post here when both answers are in."""

REVIEW = """Review of **PR #42** (cart totals rounding):

- [x] Totals stay in integer cents end to end
- [x] Tests cover the half-cent rounding cases
- [ ] Rename `calcTotal` to `calculateTotal` (nit)

Verdict: **approve**, nothing blocking."""

REVIEWS = """Open reviews:

| PR | Title | State |
|---|---|---|
| #42 | Cart totals rounding | in review |
| #45 | Coupon input validation | changes requested |"""

API = """Yes. `GET /api/cart` returns integers:

| Field | Type | Example |
|---|---|---|
| `subtotal_cents` | integer | `4990` |
| `discount_cents` | integer | `500` |
| `total_cents` | integer | `4490` |

The currency is in `currency` (ISO 4217)."""

BACKEND_STATUS = """Backend status:

| Endpoint | State |
|---|---|
| `GET /api/cart` | released |
| `POST /api/coupons` | in progress |"""

FRONTEND_STATUS = """Cart page:

- [x] New totals layout
- [x] Coupon field
- [ ] Final check against the totals API"""

DOCS = """Release notes draft for **checkout v2** is in `releases/checkout-v2.md`:

- [x] New cart totals
- [x] Coupon field
- [ ] Screenshots of the cart page"""


class Script:
    """What one agent says. Each handler returns a list of actions:
    ("reply", text) answers the sender; ("send", name, project, text) starts
    a message to another session by name."""

    def __init__(self, name, project):
        self.name = name
        self.project = project
        self.waiting = {}  # lead: who still has to answer, and where to report
        self.report_to = None  # frontend: the lead waiting for the API detail

    def handle(self, msg):
        content = msg["content"]
        lower = content.lower()
        asked = content.rstrip().endswith("?")
        if "your role in this workspace" in lower:
            # A role assignment: acknowledge it; it is not a task.
            role = content.split("Your role in this workspace:", 1)[-1].split(".", 1)[0].strip()
            return [("reply", f"Understood: I am the {role} here.")]
        handler = getattr(self, f"_{self.name}", None)
        if handler is None:
            return [("reply", "Noted.")] if msg["kind"] != "agent" or asked else []
        return handler(msg, lower, asked)

    def _lead(self, msg, lower, asked):
        if msg["kind"] == "scheduled" or "standup" in lower or "status" in lower:
            return [("reply", STANDUP)]
        if msg["kind"] == "user":
            self.waiting = {"reviewer": True, "frontend": True, "report": msg["reply_to"]}
            return [
                ("reply", PLAN),
                ("send", "reviewer", "shop",
                 "Please review PR #42 (cart totals rounding). Is anything blocking the release?"),
                ("send", "frontend", "shop", "Is the cart page ready for the new totals API?"),
            ]
        if asked:
            return [("reply", "Yes, go ahead.")]
        final = {"reviewer": "verdict", "frontend": "ready for the release"}
        if self.waiting.get(msg["sender"]) and final[msg["sender"]] in lower:
            self.waiting[msg["sender"]] = False
            if not self.waiting["reviewer"] and not self.waiting["frontend"] and self.waiting["report"]:
                report, self.waiting = self.waiting["report"], {}
                return [("to", report, None,
                         "Both checks are in: PR #42 is **approved** and the cart page is ready. "
                         "Tagging `checkout-v2-rc1`.")]
        return []

    def _reviewer(self, msg, lower, asked):
        if msg["kind"] == "agent":
            if "review" in lower:
                return [("reply", REVIEW)]
            return [("reply", "Nothing blocking from review.")] if asked else []
        return [("reply", REVIEWS)]

    def _frontend(self, msg, lower, asked):
        if msg["kind"] == "agent":
            if "cart page" in lower and asked:
                self.report_to = (msg["reply_to"], msg["project"])
                return [
                    ("reply", "The cart page is ready behind the `cart_v2` flag. "
                              "One detail is open with @backend; I will confirm shortly."),
                    ("send", "backend", "shop", "Does `GET /api/cart` return totals in cents?"),
                ]
            if msg["sender"] == "backend" and self.report_to:
                (lead, project), self.report_to = self.report_to, None
                return [("to", lead, project,
                         "Confirmed with @backend: totals arrive as integer cents. "
                         "The cart page is ready for the release.")]
            return [("reply", "Looks fine from the frontend.")] if asked else []
        return [("reply", FRONTEND_STATUS)]

    def _backend(self, msg, lower, asked):
        if msg["kind"] == "agent":
            if "cents" in lower and asked:
                return [("reply", API)]
            return [("reply", "No API change needed.")] if asked else []
        return [("reply", BACKEND_STATUS)]

    def _ops(self, msg, lower, asked):
        if msg["kind"] == "agent":
            return [("reply", "Deploys are open today 16:00-17:00.")] if asked else []
        return [("reply", OPS)]

    def _docs(self, msg, lower, asked):
        if msg["kind"] == "scheduled":
            return [("reply", "Weekly docs check: 2 pages updated, 1 broken link fixed.")]
        if msg["kind"] == "agent":
            return [("reply", "The handbook has no open questions.")] if asked else []
        return [("reply", DOCS)]


OPS = """**Production status** (remote-box)

| Service | Version | State |
|---|---|---|
| shop-web | 2.3.1 | healthy |
| shop-api | 2.3.0 | healthy |
| payments | 1.8.4 | canary at 10% |

Deploy window for checkout v2: today 16:00-17:00."""


# --- Talking to the bridge -----------------------------------------------------


def bridge(env, *args):
    result = subprocess.run(["session-bridge", *args], env=env, capture_output=True,
                            text=True, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout


def session_named(env, name, project):
    """The session id of `name` in `project`, here or on a paired laptop."""
    reply = json.loads(bridge(env, "list", "--project", project))
    for session in reply.get("sessions", []):
        if session.get("name") == name:
            return session["session_id"]
    raise RuntimeError(f"no live session named {name} in {project}")


class Agent:
    def __init__(self, kind, name, project):
        self.kind = kind
        self.name = name
        self.project = project
        self.id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"session-bridge-demo/{HOST}/{name}"))
        self.cwd = os.path.join(HOME, "work", project)
        os.makedirs(self.cwd, exist_ok=True)
        self.script = Script(name, project)
        self.env = dict(os.environ)
        if kind == "claude":
            self.env["CLAUDE_CODE_SESSION_ID"] = self.id
        else:
            self.env["CODEX_THREAD_ID"] = self.id
        self.lock = threading.Lock()

    def act(self, msg, reply):
        """Runs the script for one message; `reply` answers the sender."""
        log(self.name, f"received ({msg['kind']}): {msg['content'][:80]!r}")
        with self.lock:
            actions = self.script.handle(msg)
        for action in actions:
            time.sleep(DELAY)
            try:
                kind, rest = action[0], action[1:]
                if kind == "reply":
                    reply(rest[0])
                elif kind == "send":
                    name, project, text = rest
                    target = session_named(self.env, name, project)
                    bridge(self.env, "send", target, text, "--project", project)
                elif kind == "to":
                    target, project, text = rest
                    args = ["send", target, text] + (["--project", project] if project else [])
                    bridge(self.env, *args)
                log(self.name, f"sent {action[0]} {action[1] if action[0] != 'reply' else ''}")
            except (RuntimeError, OSError, subprocess.SubprocessError, ValueError) as error:
                log(self.name, f"could not {action[0]}: {error}")

    def reply_by_cli(self, msg):
        def reply(text):
            args = ["send", msg["reply_to"], text]
            if msg["kind"] == "agent" and msg["project"]:
                args += ["--project", msg["project"]]
            bridge(self.env, *args)
        return reply

    # Claude ------------------------------------------------------------------

    def run_claude(self):
        sockets = os.path.join(HOME, ".claude", "demo")
        sessions = os.path.join(HOME, ".claude", "sessions")
        os.makedirs(sockets, mode=0o700, exist_ok=True)
        os.makedirs(sessions, exist_ok=True)
        inbox = os.path.join(sockets, f"{self.name}.sock")
        if os.path.exists(inbox):
            os.unlink(inbox)
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(inbox)
        os.chmod(inbox, 0o600)  # the bridge accepts only a private socket
        server.listen(16)
        record = os.path.join(sessions, f"{os.getpid()}.json")
        with open(record, "w") as f:
            json.dump({"pid": os.getpid(), "sessionId": self.id, "cwd": self.cwd,
                       "messagingSocketPath": inbox, "name": self.name,
                       "status": "idle", "kind": "interactive"}, f)
        self.cleanup = [record, inbox]
        log(self.name, f"Claude session {self.id} in {self.project}")
        while True:
            conn, _ = server.accept()
            threading.Thread(target=self.claude_frame, args=(conn, inbox), daemon=True).start()

    def claude_frame(self, conn, inbox):
        try:
            with conn:
                frame = json.loads(conn.makefile().readline())
        except (OSError, ValueError) as error:
            log(self.name, f"bad frame: {error}")
            return
        if frame.get("type") != "user" or not str(frame.get("from", "")).startswith("uds:"):
            return
        back = frame["from"][len("uds:"):]
        own = f"uds:{inbox}"

        def send(value):
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as out:
                out.settimeout(10)
                out.connect(back)
                out.sendall(json.dumps(value).encode() + b"\n")

        try:
            send({"type": "control", "action": "peer_message_status", "from": own,
                  "orig_msg_id": frame.get("msg_id"), "status": "delivered"})
        except OSError as error:
            log(self.name, f"could not confirm delivery: {error}")

        def reply(text):
            message_id = str(uuid.uuid4())
            send({"type": "user", "message": {"role": "user", "content": text}, "from": own,
                  "uuid": message_id, "msg_id": message_id})

        self.act(parse(frame["message"]["content"]), reply)

    # Codex -------------------------------------------------------------------

    def run_codex(self):
        codex = os.path.join(HOME, ".codex")
        day = time.strftime("%Y/%m/%d")
        rollouts = os.path.join(codex, "sessions", day)
        inbox = os.path.join(codex, "demo-inbox", self.id)
        os.makedirs(rollouts, exist_ok=True)
        os.makedirs(inbox, exist_ok=True)
        stamp = time.strftime("%Y-%m-%dT%H-%M-%S")
        rollout = os.path.join(rollouts, f"rollout-{stamp}-{self.id}.jsonl")
        meta = {"timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "type": "session_meta",
                "payload": {"id": self.id, "cwd": self.cwd, "source": "cli",
                            "originator": "codex_cli_rs"}}
        # Kept open for the agent's life: an open rollout file is how the
        # bridge sees a live Codex session.
        self.rollout = open(rollout, "w")
        self.rollout.write(json.dumps(meta) + "\n")
        self.rollout.flush()
        with open(os.path.join(codex, "session_index.jsonl"), "a") as f:
            f.write(json.dumps({"id": self.id, "thread_name": self.name,
                                "updated_at": meta["timestamp"]}) + "\n")
        self.cleanup = [inbox]
        log(self.name, f"Codex thread {self.id} in {self.project}")
        while True:
            for entry in sorted(os.listdir(inbox)):
                if not entry.endswith(".txt"):
                    continue
                path = os.path.join(inbox, entry)
                with open(path) as f:
                    text = f.read()
                os.unlink(path)
                msg = parse(text)
                threading.Thread(target=self.act, args=(msg, self.reply_by_cli(msg)),
                                 daemon=True).start()
            time.sleep(0.5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=["claude", "codex"], required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--project", required=True)
    args = parser.parse_args()
    agent = Agent(args.kind, args.name, args.project)

    def stop(*_):
        for path in getattr(agent, "cleanup", []):
            try:
                if os.path.isdir(path):
                    for entry in os.listdir(path):
                        os.unlink(os.path.join(path, entry))
                    os.rmdir(path)
                else:
                    os.unlink(path)
            except OSError:
                pass
        sys.exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    agent.run_claude() if args.kind == "claude" else agent.run_codex()


if __name__ == "__main__":
    main()
