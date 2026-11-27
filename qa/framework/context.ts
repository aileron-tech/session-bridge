// What an action or scenario gets: the page, the test bridge (and a second
// one on demand), steps for the report, and small helpers.
import { Bridge, type Json } from "./bridge";
import type { Page } from "./page";
import type { Browser } from "./browser";
import { QaError } from "./page";

/** Thrown by an action that cannot run here (for example, it needs QA_LAN=1). */
export class Skip extends Error {}

export type Action = (ctx: Ctx) => Promise<void>;
export type Domain = { name: string; actions: Record<string, Action> };
export type Scenario = { name: string; description: string; run: Action };

export class Ctx {
  private static loads = 0;
  readonly steps: string[] = [];
  private second?: Bridge;

  private phone?: Page;

  constructor(
    readonly page: Page,
    readonly bridge: Bridge,
    private shots: (name: string) => string,
    private keepShots: boolean,
    private browser?: Browser,
  ) {}

  /** A second page for the phone app (headless only; Skip under Arc); closed after the action. */
  async phonePage(): Promise<Page> {
    if (!this.browser) throw new Skip("no browser for a second page");
    this.phone ??= await this.browser.secondPage();
    return this.phone;
  }

  /** A named step: recorded in the report, screenshot with QA_SHOTS=all. */
  async step<T>(name: string, fn: () => Promise<T>): Promise<T> {
    this.steps.push(name);
    const value = await fn();
    if (this.keepShots) await this.page.screenshot(this.shots(`${this.steps.length}-${name}`));
    return value;
  }

  /** Opens the admin page fresh on a tab (chats, cron, peers, projects, run, roles). */
  async open(tab = "chats") {
    // A fresh load each time: a new ?qa= query (the server ignores it), since
    // the same address with its #token fragment would not reload.
    const url = new URL(await this.bridge.admin());
    url.searchParams.set("qa", String(++Ctx.loads));
    await this.page.goto(url.toString());
    await this.page.visible(".tabs__tab");
    await this.page.click(`.tabs__tab[data-tab="${tab}"]`);
    await this.page.visible(`#tab-${tab}`);
  }

  /** Waits until a confirm() or alert() starting with `prefix` was answered. */
  async dialog(prefix: string) {
    await this.eventually(`a dialog "${prefix}..."`, async () => (await this.page.dialogs()).some((d) => d.startsWith(prefix)), 5_000);
  }

  /** Opens a sidebar item of a split tab by its data-qa key (nav:<tab>:<key>,
   * e.g. "new", "all", "role:<name>", "project:<p>"); waits for it to appear. */
  async nav(tab: string, key: string) {
    await this.page.click(`[data-qa="nav:${tab}:${key}"]`);
  }

  /** Skips unless QA_LAN=1: the action announces this bridge on the real network (mDNS). */
  needsLan(what: string) {
    if (process.env.QA_LAN !== "1") throw new Skip(`${what} announces on the local network; run with QA_LAN=1`);
  }

  /** A unique, readable name (letters, digits, '-'), for isolation between actions. */
  uid(prefix: string): string {
    return `${prefix}-${Math.random().toString(36).slice(2, 7)}`;
  }

  /** A second bridge (peers), started once and stopped with the run. */
  async secondBridge(): Promise<Bridge> {
    this.second ??= await Bridge.start("b");
    return this.second;
  }

  async cleanup() {
    await this.phone?.close().catch(() => {});
    await this.second?.stop();
  }

  expect(ok: unknown, message: string): void {
    if (!ok) throw new QaError(message);
  }

  /** Polls the bridge until `fn` returns a truthy value. */
  async eventually<T>(message: string, fn: () => Promise<T | undefined | null | false>, timeoutMs = 15_000): Promise<T> {
    const deadline = Date.now() + timeoutMs;
    let last: unknown;
    for (;;) {
      try {
        const value = await fn();
        if (value) return value as T;
        last = value;
      } catch (e) {
        last = e;
      }
      if (Date.now() > deadline) throw new QaError(`timed out: ${message} (last: ${last instanceof Error ? last.message : JSON.stringify(last)})`);
      await Bun.sleep(250);
    }
  }

  /** The page's API reply, failing on {"result": "error"}. */
  async pageOk(request: Json): Promise<Json> {
    const reply = await this.bridge.page(request);
    if (reply.result === "error") throw new QaError(`${request.method}: ${reply.message}`);
    return reply;
  }
}
