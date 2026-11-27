// A separate test bridge: its own HOME and SESSION_BRIDGE_STATE under a short
// /tmp folder (state chmod 700), stand-in terminal, Claude, Codex, crontab
// and ssh, no notifications. Never the installed service, the user's real
// crontab or state. `stop()` ends every process it started and removes the
// folder, also after a failure.
import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, mkdtempSync, readdirSync } from "node:fs";
import { createConnection } from "node:net";
import { join, resolve } from "node:path";
import { CLAUDE, CODEX, CRONTAB, SSH, TERMINAL, ZSHRC_ALIASES } from "./standins";

const REPO = resolve(import.meta.dir, "../..");

/** The bridge executable: SB_BIN, else the repo's debug build. */
export function bridgeBinary(): string {
  const bin = process.env.SB_BIN ?? join(REPO, "target/debug/session-bridge");
  if (!existsSync(bin)) throw new Error(`No bridge executable at ${bin}: run \`cargo build\` or set SB_BIN`);
  return bin;
}

export type Json = any;

export class Bridge {
  readonly home: string;
  readonly state: string;
  readonly bin: string;
  private daemon?: ReturnType<typeof Bun.spawn>;
  private children: ReturnType<typeof Bun.spawn>[] = [];
  private adminUrl?: string;

  private constructor(
    readonly name: string,
    readonly dir: string,
  ) {
    this.home = join(dir, "h");
    this.state = join(dir, "s");
    this.bin = bridgeBinary();
  }

  /** Creates the folder and stand-ins and starts the daemon. */
  static async start(name = "a"): Promise<Bridge> {
    // Unix socket paths must stay short: /tmp/sbqa-XXXXXX/<name>/s/control.sock
    const root = mkdtempSync("/tmp/sbqa-");
    const bridge = new Bridge(name, join(root, name));
    try {
      bridge.prepare();
      await bridge.launch();
      return bridge;
    } catch (e) {
      await bridge.stop();
      throw e;
    }
  }

  private prepare() {
    for (const d of [this.dir, join(this.dir, "bin"), join(this.home, ".claude/sessions"), join(this.home, ".codex/sessions"), join(this.home, "work"), join(this.dir, "received")]) {
      mkdirSync(d, { recursive: true });
    }
    mkdirSync(this.state, { recursive: true });
    chmodSync(this.state, 0o700);
    const scripts: Record<string, string> = { claude: CLAUDE, codex: CODEX, terminal: TERMINAL, crontab: CRONTAB, ssh: SSH };
    for (const [file, text] of Object.entries(scripts)) {
      const path = join(this.dir, "bin", file);
      writeFileSync(path, text);
      chmodSync(path, 0o755);
    }
    writeFileSync(join(this.home, ".zshrc"), ZSHRC_ALIASES);
  }

  /** The environment of this bridge and of everything it starts. */
  get env(): Record<string, string> {
    return {
      PATH: `${join(this.dir, "bin")}:/usr/bin:/bin:/usr/sbin:/sbin`,
      HOME: this.home,
      SESSION_BRIDGE_STATE: this.state,
      CLAUDE_CONFIG_DIR: join(this.home, ".claude"),
      CODEX_HOME: join(this.home, ".codex"),
      SESSION_BRIDGE_TERMINAL: join(this.dir, "bin/terminal"),
      SESSION_BRIDGE_CLAUDE_BIN: join(this.dir, "bin/claude"),
      SESSION_BRIDGE_CODEX_BIN: join(this.dir, "bin/codex"),
      SESSION_BRIDGE_CRONTAB: join(this.dir, "bin/crontab"),
      SESSION_BRIDGE_SSH: join(this.dir, "bin/ssh"),
      SESSION_BRIDGE_NOTIFY: "0",
      SESSION_BRIDGE_RELEASES_URL: "http://127.0.0.1:9/none",
      SB_QA_DIR: this.dir,
    };
  }

  private async launch() {
    this.daemon = Bun.spawn([this.bin, "daemon"], {
      env: this.env,
      cwd: this.home,
      stdout: Bun.file(join(this.dir, "daemon.out")),
      stderr: Bun.file(join(this.dir, "daemon.err")),
    });
    const deadline = Date.now() + 20_000;
    for (;;) {
      try {
        const reply = await this.rpc({ method: "health" }, 2_000);
        if (reply.result !== "error") break;
      } catch {
        // Not listening yet.
      }
      if (this.daemon.exitCode !== null || Date.now() > deadline) {
        throw new Error(`Test bridge did not start: ${this.log().slice(-800)}`);
      }
      await Bun.sleep(150);
    }
  }

  /** The daemon's stderr, for failure reports. */
  log(): string {
    const file = join(this.dir, "daemon.err");
    return existsSync(file) ? readFileSync(file, "utf8") : "";
  }

  /** One request on the control socket (as the CLI or an agent would send it). */
  rpc(request: Json, timeoutMs = 30_000): Promise<Json> {
    return new Promise((resolvePromise, reject) => {
      const socket = createConnection({ path: join(this.state, "control.sock") });
      let data = "";
      const timer = setTimeout(() => {
        socket.destroy();
        reject(new Error(`rpc ${request.method} timed out`));
      }, timeoutMs);
      socket.on("connect", () => socket.write(JSON.stringify(request) + "\n"));
      socket.on("data", (chunk) => {
        data += chunk.toString();
        if (data.includes("\n")) {
          clearTimeout(timer);
          socket.end();
          try {
            resolvePromise(JSON.parse(data.slice(0, data.indexOf("\n"))));
          } catch (e) {
            reject(e);
          }
        }
      });
      socket.on("error", (e) => {
        clearTimeout(timer);
        reject(e);
      });
    });
  }

  /** The admin page's address, with its token in the fragment. */
  async admin(): Promise<string> {
    if (!this.adminUrl) {
      const reply = await this.rpc({ method: "admin" });
      if (reply.result !== "admin") throw new Error(`No admin page: ${JSON.stringify(reply)}`);
      this.adminUrl = reply.url as string;
    }
    return this.adminUrl;
  }

  /** A request through the admin page's API (as the user), for setup and checks. */
  async page(request: Json): Promise<Json> {
    const url = new URL(await this.admin());
    const token = url.hash.replace("#token=", "");
    const response = await fetch(`${url.origin}/api/call`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
    if (!response.ok) throw new Error(`page ${request.method}: HTTP ${response.status} ${await response.text()}`);
    return response.json();
  }

  /** A project folder under the test home. */
  project(name: string): string {
    const dir = join(this.home, "work", name);
    mkdirSync(dir, { recursive: true });
    return dir;
  }

  /** Starts a stand-in Claude session `name` in project `project`; resolves when the bridge lists it. */
  async addClaude(name: string, project: string, opts: { mute?: boolean } = {}): Promise<Json> {
    const child = Bun.spawn([join(this.dir, "bin/claude"), "-n", name, "hello"], {
      cwd: this.project(project),
      env: { ...this.env, ...(opts.mute ? { SB_QA_MUTE: "1" } : {}) },
      stdout: "ignore",
      stderr: "ignore",
    });
    this.children.push(child);
    return this.waitSession(name);
  }

  /** Starts a stand-in Codex thread named `name` in `project`. */
  async addCodex(name: string, project: string): Promise<Json> {
    const child = Bun.spawn([join(this.dir, "bin/codex"), "hello"], {
      cwd: this.project(project),
      env: { ...this.env, SB_QA_NAME: name, SB_QA_THREAD: crypto.randomUUID() },
      stdout: "ignore",
      stderr: "ignore",
    });
    this.children.push(child);
    return this.waitSession(name, 45_000);
  }

  async sessions(project?: string): Promise<Json[]> {
    const reply = await this.rpc({ method: "list", sender: null, project: project ?? null });
    return reply.sessions ?? [];
  }

  async waitSession(name: string, timeoutMs = 20_000): Promise<Json> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const found = (await this.sessions()).find((s) => s.name === name);
      if (found) return found;
      if (Date.now() > deadline) throw new Error(`Session ${name} not listed in ${timeoutMs / 1000} s`);
      await Bun.sleep(300);
    }
  }

  /** Ends the stand-in session `name` (its process), as if it exited. */
  endSession(name: string) {
    // Started ones carry flags before -n: match anything in between.
    Bun.spawnSync(["pkill", "-f", `${join(this.dir, "bin/claude")} .*-n ${name} `]);
  }

  async waitGone(name: string, timeoutMs = 20_000) {
    const deadline = Date.now() + timeoutMs;
    while ((await this.sessions()).some((s) => s.name === name)) {
      if (Date.now() > deadline) throw new Error(`Session ${name} still listed after ${timeoutMs / 1000} s`);
      await Bun.sleep(300);
    }
  }

  /** What a stand-in session received (one JSON object per line). */
  received(file: string): Json[] {
    const path = join(this.dir, "received", `${file}.log`);
    if (!existsSync(path)) return [];
    return readFileSync(path, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
  }

  async waitReceived(file: string, contains: string, timeoutMs = 20_000): Promise<Json> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const hit = this.received(file).find((r) => (r.message ?? r.first_prompt ?? "").includes(contains));
      if (hit) return hit;
      if (Date.now() > deadline) throw new Error(`${file} did not receive "${contains}" in ${timeoutMs / 1000} s`);
      await Bun.sleep(250);
    }
  }

  receivedFiles(): string[] {
    const dir = join(this.dir, "received");
    return existsSync(dir) ? readdirSync(dir) : [];
  }

  /** The stand-in crontab's table. */
  crontab(): string {
    const path = join(this.dir, "crontab.txt");
    return existsSync(path) ? readFileSync(path, "utf8") : "";
  }

  /** Ends the daemon and every stand-in, then removes the folder. */
  async stop() {
    try {
      if (this.daemon && this.daemon.exitCode === null) {
        await this.rpc({ method: "shutdown" }, 5_000).catch(() => {});
        const deadline = Date.now() + 5_000;
        while (this.daemon.exitCode === null && Date.now() < deadline) await Bun.sleep(100);
        if (this.daemon.exitCode === null) this.daemon.kill(9);
      }
    } finally {
      for (const child of this.children) child.kill(9);
      // Launched sessions and their terminals run from this folder.
      Bun.spawnSync(["pkill", "-9", "-f", this.dir]);
      rmSync(resolve(this.dir, ".."), { recursive: true, force: true });
    }
  }
}
