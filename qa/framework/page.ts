// One browser tab, whichever backend drives it (Arc locally, headless Chrome
// in CI). The backend supplies four primitives: run a script in the page,
// navigate, screenshot, close. Everything else is here and the same for
// both: waits, JS clicks, typing, reading text, and a page-side collector of
// console errors and confirm()/alert() texts (answered without a dialog).

export class QaError extends Error {}
/** The backend refused to work (for example Arc's JavaScript from Apple Events is off): the run stops. */
export class Fatal extends Error {}

type Predicate = string | ((...args: any[]) => any);
export type Target = { selector?: string; scope?: string; text?: string; tag?: string; within?: string };

/** Installed after every load: records errors and answers dialogs. */
const COLLECTOR = `(() => {
  if (window.__qa) return true;
  window.__qa = { errors: [], dialogs: [], answer: true };
  const record = (kind) => (...args) => {
    window.__qa.dialogs.push(String(args[0] ?? ""));
    return kind === "confirm" ? window.__qa.answer : kind === "prompt" ? null : undefined;
  };
  window.confirm = record("confirm");
  window.alert = record("alert");
  window.prompt = record("prompt");
  const error = console.error.bind(console);
  console.error = (...args) => { window.__qa.errors.push("console.error: " + args.map(String).join(" ")); error(...args); };
  window.addEventListener("error", (e) => window.__qa.errors.push("exception: " + (e.error?.stack ?? e.message)));
  window.addEventListener("unhandledrejection", (e) => window.__qa.errors.push("unhandled rejection: " + (e.reason?.stack ?? e.reason)));
  return true;
})()`;

export abstract class Page {
  /** Runs a script (an expression) in the page; returns its JSON value. */
  protected abstract run(expression: string): Promise<any>;
  protected abstract navigate(url: string): Promise<void>;
  abstract screenshot(path: string): Promise<void>;
  abstract close(): Promise<void>;
  /** Errors the backend saw itself (CDP: before the collector was installed). */
  protected backendErrors(): string[] {
    return [];
  }

  /** Runs `fn(...args)` in the page; returns its JSON value. */
  async evaluate<T = any>(fn: Predicate, ...args: any[]): Promise<T> {
    const expression = typeof fn === "string" ? fn : `(${fn.toString()})(...${JSON.stringify(args)})`;
    return this.run(expression);
  }

  /** Loads `url` and installs the collector. The old document is marked
   * first, so the wait sees the new one (the page drops its own query and
   * #token from the address, so the URL cannot tell). */
  async goto(url: string) {
    await this.run("window.__qaOld = true").catch(() => {});
    await this.navigate(url);
    await this.waitFor(() => document.readyState === "complete" && !(window as any).__qaOld && location.protocol.startsWith("http"), {
      message: `load ${url}`,
      timeout: 20_000,
    });
    await this.run(COLLECTOR);
  }

  /** How the next confirm() is answered. */
  async answerDialogs(accept: boolean) {
    await this.evaluate((a: boolean) => window.__qa && (window.__qa.answer = a), accept);
  }

  async dialogs(): Promise<string[]> {
    return (await this.evaluate(() => window.__qa?.dialogs ?? [])) as string[];
  }

  /** Console errors and uncaught exceptions since the last call. */
  async takeErrors(): Promise<string[]> {
    const page = await this.evaluate(() => {
      const errors = window.__qa?.errors ?? [];
      if (window.__qa) window.__qa.errors = [];
      return errors;
    }).catch(() => [] as string[]);
    return [...this.backendErrors(), ...page];
  }

  /** Polls until `fn(...args)` is truthy in the page; returns that value. */
  async waitFor<T = any>(fn: Predicate, opts: { timeout?: number; message?: string; args?: any[] } = {}): Promise<T> {
    const deadline = Date.now() + (opts.timeout ?? 10_000);
    let last: unknown;
    for (;;) {
      try {
        const value = await this.evaluate(fn, ...(opts.args ?? []));
        if (value) return value as T;
        last = value;
      } catch (e) {
        last = e;
      }
      if (Date.now() > deadline) {
        throw new QaError(`timed out waiting for ${opts.message ?? String(fn)} (last: ${last instanceof Error ? last.message : JSON.stringify(last)})`);
      }
      await Bun.sleep(150);
    }
  }

  async visible(selector: string, timeout = 10_000) {
    await this.waitFor(
      (sel: string) => {
        const el = document.querySelector(sel) as HTMLElement | null;
        return !!el && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== "hidden";
      },
      { args: [selector], timeout, message: `visible ${selector}` },
    );
  }

  async isVisible(selector: string): Promise<boolean> {
    return this.evaluate((sel: string) => {
      const el = document.querySelector(sel) as HTMLElement | null;
      return !!el && el.getClientRects().length > 0;
    }, selector);
  }

  async hidden(selector: string, timeout = 10_000) {
    await this.waitFor(
      (sel: string) => {
        const el = document.querySelector(sel) as HTMLElement | null;
        return !el || el.getClientRects().length === 0;
      },
      { args: [selector], timeout, message: `hidden ${selector}` },
    );
  }

  /** Clicks the first shown, enabled element for `target`: a selector, or text inside a scope. */
  async click(target: string | Target) {
    const t = typeof target === "string" ? { selector: target } : target;
    await this.waitFor(
      (t: Target) => {
        const isShown = (el: Element) => el.getClientRects().length > 0;
        let el: Element | undefined | null;
        if (t.selector) {
          el = [...document.querySelectorAll(t.selector)].find(isShown);
        } else {
          const scope = document.querySelector(t.scope ?? "body");
          if (!scope) return false;
          const norm = (s: string) => s.replace(/\s+/g, " ").trim();
          const candidates = [...scope.querySelectorAll(t.tag ?? "button, a, [role=button], li, tr")].filter(isShown);
          el = candidates.find((c) => norm(c.textContent ?? "") === t.text) ?? candidates.find((c) => norm(c.textContent ?? "").includes(t.text!));
          if (el && t.within) el = el.querySelector(t.within);
        }
        if (!el || !isShown(el) || (el as HTMLButtonElement).disabled) return false;
        (el as HTMLElement).scrollIntoView({ block: "center" });
        (el as HTMLElement).focus?.();
        (el as HTMLElement).click();
        return true;
      },
      { args: [t], message: `clickable ${t.selector ?? `"${t.text}"${t.scope ? ` in ${t.scope}` : ""}${t.within ? ` -> ${t.within}` : ""}`}` },
    );
  }

  /** Replaces a field's text, firing input events as typing does. */
  async type(selector: string, text: string) {
    await this.visible(selector);
    await this.evaluate(
      (sel: string, value: string) => {
        const el = document.querySelector(sel) as HTMLInputElement | HTMLTextAreaElement;
        el.focus();
        el.value = value;
        el.dispatchEvent(new InputEvent("input", { bubbles: true, data: value, inputType: "insertText" }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
      },
      selector,
      text,
    );
  }

  /** Chooses an option of a <select> by value, or else by its visible label. */
  async select(selector: string, valueOrLabel: string) {
    await this.visible(selector);
    await this.waitFor(
      (sel: string, want: string) => {
        const el = document.querySelector(sel) as HTMLSelectElement;
        const option = [...el.options].find((o) => o.value === want) ?? [...el.options].find((o) => o.text.trim() === want || o.text.includes(want));
        if (!option) return false;
        el.focus();
        el.value = option.value;
        el.dispatchEvent(new Event("input", { bubbles: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
        return true;
      },
      { args: [selector, valueOrLabel], message: `option "${valueOrLabel}" in ${selector}` },
    );
  }

  async text(selector: string): Promise<string> {
    return this.evaluate((sel: string) => (document.querySelector(sel)?.textContent ?? "").replace(/\s+/g, " ").trim(), selector);
  }

  async value(selector: string): Promise<string> {
    return this.evaluate((sel: string) => (document.querySelector(sel) as HTMLInputElement)?.value ?? "", selector);
  }

  /** Waits until `selector`'s text contains (or matches) `want`. */
  async textContains(selector: string, want: string | RegExp, timeout = 10_000) {
    const pattern = want instanceof RegExp ? { source: want.source, flags: want.flags } : null;
    await this.waitFor(
      (sel: string, text: string, re: { source: string; flags: string } | null) => {
        const got = (document.querySelector(sel)?.textContent ?? "").replace(/\s+/g, " ");
        return re ? new RegExp(re.source, re.flags).test(got) : got.includes(text);
      },
      { args: [selector, typeof want === "string" ? want : "", pattern], timeout, message: `${selector} contains ${want}` },
    );
  }
}

declare global {
  interface Window {
    __qa?: { errors: string[]; dialogs: string[]; answer: boolean };
  }
}
