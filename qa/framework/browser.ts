// Backends. Locally (the default): ONE new tab in the user's existing front
// Arc window, driven over Apple Events, closed at the end, also on failure;
// never another window and never another tab. In CI (QA_DRIVER=cdp, or CI
// set): headless Chrome with its own profile, no window.
import { mkdtempSync, readFileSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Cdp } from "./cdp";
import { Fatal, Page } from "./page";
import { Skip } from "./context";

export interface Browser {
  /** The single tab of this run, opened on `url` (Arc refuses about:blank). */
  page(url: string): Promise<Page>;
  /** A second, separate page (the phone app): headless Chrome only, which
   * ignores certificate errors in its own throwaway profile. Arc: Skip. */
  secondPage(): Promise<Page>;
  close(): Promise<void>;
}

export function driverName(): "arc" | "cdp" {
  const chosen = process.env.QA_DRIVER;
  if (chosen === "arc" || chosen === "cdp") return chosen;
  return process.env.CI ? "cdp" : "arc";
}

export async function launch(): Promise<Browser> {
  return driverName() === "cdp" ? CdpBrowser.launch() : ArcBrowser.open();
}

// ---------- Arc (local default) ----------

/** JXA over osascript; `args` reach the script as argv, so nothing needs escaping. */
async function jxa(source: string, ...args: string[]): Promise<string> {
  const child = Bun.spawn(["osascript", "-l", "JavaScript", "-e", source, "--", ...args], { stdout: "pipe", stderr: "pipe" });
  let hung = false;
  const timer = setTimeout(() => {
    hung = true;
    child.kill();
  }, 20_000);
  const [out, err, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
  clearTimeout(timer);
  // Arc not answering within 20 s (a prompt waiting, scripting refused): stop the run.
  if (hung) throw new Fatal("Arc did not answer an Apple Event within 20 s (is a prompt waiting in Arc?)");
  if (code !== 0) throw new Error(`osascript: ${err.trim() || `exit ${code}`}`);
  return out.trim();
}

const TAB = `const arc = Application("Arc");
const tab = arc.windows.byId(argv[0]).tabs.byId(argv[1]);`;

class ArcPage extends Page {
  constructor(
    private windowId: string,
    private tabId: string,
  ) {
    super();
  }
  protected async run(expression: string): Promise<any> {
    // The page answers with JSON text: execute returns strings reliably.
    const wrapped = `(() => { try { return JSON.stringify({ ok: (${expression}) }); } catch (e) { return JSON.stringify({ error: String(e && e.stack || e) }); } })()`;
    let out: string;
    try {
      out = await jxa(`function run(argv) { ${TAB} return tab.execute({ javascript: argv[2] }); }`, this.windowId, this.tabId, wrapped);
    } catch (e) {
      // A refusal from Arc itself (scripting disabled, not authorized, tab gone) ends the run.
      const text = String(e);
      if (/Apple ?Events|JavaScript|-1743|not allowed|turned off|Can't get object|Invalid index/i.test(text)) throw new Fatal(text);
      throw e;
    }
    if (!out) return undefined;
    const reply = JSON.parse(out);
    if (reply.error) throw new Error(`page script failed: ${reply.error}`);
    return reply.ok;
  }
  protected async navigate(url: string) {
    await jxa(`function run(argv) { ${TAB} tab.url = argv[2]; }`, this.windowId, this.tabId, url);
    // Wait for the new document to start loading before polling its state.
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      const loading = await jxa(`function run(argv) { ${TAB} return String(tab.loading()); }`, this.windowId, this.tabId).catch(() => "true");
      if (loading === "false") break;
      await Bun.sleep(150);
    }
  }
  /** Only on failure: a capture of this Arc window's area of the screen. */
  async screenshot(path: string) {
    const id = await jxa(
      `ObjC.import("CoreGraphics");
function run(argv) {
  const name = Application("Arc").windows.byId(argv[0]).name();
  const list = ObjC.deepUnwrap($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0)) || [];
  const hit = list.find((w) => w.kCGWindowOwnerName === "Arc" && w.kCGWindowName === name) || list.find((w) => w.kCGWindowOwnerName === "Arc" && w.kCGWindowLayer === 0);
  return hit ? String(hit.kCGWindowNumber) : "";
}`,
      this.windowId,
    );
    if (!id) throw new Error("Arc window not found on screen");
    const done = Bun.spawnSync(["screencapture", "-x", "-o", `-l${id}`, path]);
    if (done.exitCode !== 0) throw new Error(`screencapture: ${done.stderr.toString().trim()}`);
  }
  async close() {
    await jxa(`function run(argv) { ${TAB} tab.close(); }`, this.windowId, this.tabId);
  }
}

class ArcBrowser implements Browser {
  private tab?: ArcPage;
  private constructor(private windowId: string) {}

  static async open(): Promise<ArcBrowser> {
    const windowId = await jxa(`function run() { const arc = Application("Arc"); if (arc.windows.length === 0) throw new Error("No Arc window open"); return arc.windows[0].id(); }`);
    return new ArcBrowser(windowId);
  }

  /** Makes the run's one tab in the front window (made once, reused). */
  async page(url: string): Promise<Page> {
    if (!this.tab) {
      const tabId = await jxa(
        `function run(argv) {
  const arc = Application("Arc");
  const win = arc.windows.byId(argv[0]);
  const tab = arc.Tab({ url: argv[1] });
  win.tabs.push(tab);
  return win.tabs[win.tabs.length - 1].id();
}`,
        this.windowId,
        url,
      );
      console.error(`arc: QA tab ${tabId} in window ${this.windowId} (${url.replace(/#.*/, "")})`);
      this.tab = new ArcPage(this.windowId, tabId);
    }
    return this.tab;
  }

  async secondPage(): Promise<Page> {
    throw new Skip("a second page (the phone app) needs QA_DRIVER=cdp: Arc does not trust the bridge's phone root");
  }

  async close() {
    await this.tab?.close().catch((e) => console.error(`Could not close the QA tab: ${e}`));
    this.tab = undefined;
  }
}

// ---------- headless Chrome over CDP (CI) ----------

const CANDIDATES = [
  process.env.QA_CHROME,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
].filter(Boolean) as string[];

class CdpPage extends Page {
  private errors: string[] = [];
  constructor(
    private cdp: Cdp,
    private sessionId: string,
    readonly targetId: string,
  ) {
    super();
  }
  private send<T = any>(method: string, params: object = {}) {
    return this.cdp.send<T>(method, params, this.sessionId);
  }
  async init() {
    this.cdp.on("Runtime.exceptionThrown", (p, s) => {
      if (s === this.sessionId) this.errors.push(`exception: ${p.exceptionDetails?.exception?.description ?? p.exceptionDetails?.text}`);
    });
    // confirm() is replaced by the collector; a dialog that still opens is answered.
    this.cdp.on("Page.javascriptDialogOpening", (_p, s) => {
      if (s === this.sessionId) this.send("Page.handleJavaScriptDialog", { accept: true }).catch(() => {});
    });
    await this.send("Page.enable");
    await this.send("Runtime.enable");
    await this.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  }
  protected backendErrors() {
    const taken = this.errors;
    this.errors = [];
    return taken;
  }
  protected async run(expression: string): Promise<any> {
    const result = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true, userGesture: true });
    if (result.exceptionDetails) throw new Error(`page script failed: ${result.exceptionDetails.exception?.description ?? result.exceptionDetails.text}`);
    return result.result.value;
  }
  protected async navigate(url: string) {
    await this.send("Page.navigate", { url });
  }
  async screenshot(path: string) {
    const { data } = await this.send("Page.captureScreenshot", { format: "png" });
    await Bun.write(path, Buffer.from(data, "base64"));
  }
  async close() {
    await this.cdp.send("Target.closeTarget", { targetId: this.targetId }).catch(() => {});
  }
}

class CdpBrowser implements Browser {
  private tab?: CdpPage;
  private constructor(
    private process: ReturnType<typeof Bun.spawn>,
    private profile: string,
    private cdp: Cdp,
  ) {}

  static async launch(): Promise<CdpBrowser> {
    const chrome = CANDIDATES.find((path) => existsSync(path));
    if (!chrome) throw new Error(`No Chrome found; set QA_CHROME (tried ${CANDIDATES.join(", ")})`);
    const profile = mkdtempSync(join(tmpdir(), "sbqa-chrome-"));
    const child = Bun.spawn(
      [chrome, "--headless=new", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check",
        "--disable-extensions", "--disable-background-networking", "--disable-sync", "--disable-component-update", "--password-store=basic",
        "--use-mock-keychain", "--window-size=1440,900", "about:blank"],
      { stdout: "ignore", stderr: "ignore" },
    );
    const portFile = join(profile, "DevToolsActivePort");
    const deadline = Date.now() + 20_000;
    while (!existsSync(portFile) || readFileSync(portFile, "utf8").split("\n").length < 2) {
      if (Date.now() > deadline) {
        child.kill();
        rmSync(profile, { recursive: true, force: true });
        throw new Error("Chrome did not open its debugging port in 20 s");
      }
      await Bun.sleep(100);
    }
    const [port, path] = readFileSync(portFile, "utf8").trim().split("\n");
    return new CdpBrowser(child, profile, await Cdp.connect(`ws://127.0.0.1:${port}${path}`));
  }

  private async target(): Promise<CdpPage> {
    const { targetId } = await this.cdp.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await this.cdp.send("Target.attachToTarget", { targetId, flatten: true });
    const page = new CdpPage(this.cdp, sessionId, targetId);
    await page.init();
    return page;
  }

  async page(url: string): Promise<Page> {
    if (!this.tab) {
      this.tab = await this.target();
      await this.tab.goto(url);
    }
    return this.tab;
  }

  /** A second target in this headless profile, which accepts the bridge's own certificates. */
  async secondPage(): Promise<Page> {
    const page = await this.target();
    await this.cdp.send("Security.setIgnoreCertificateErrors", { ignore: true });
    return page;
  }

  async close() {
    await this.cdp.send("Browser.close", {}, undefined, 5_000).catch(() => {});
    this.cdp.close();
    this.process.kill();
    await this.process.exited;
    rmSync(this.profile, { recursive: true, force: true });
  }
}
