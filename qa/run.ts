// Runs QA scenarios and domain actions against a test bridge in headless Chrome.
//   bun qa/run.ts                      every action and scenario
//   bun qa/run.ts cron                 every action of a domain
//   bun qa/run.ts cron.edit chats.dm.send team-kickoff
// Prints a pass/fail table; exits 1 on any failure or page console error.
// Env: QA_DRIVER=arc|cdp (default arc locally: one tab in the front Arc
// window; cdp in CI: headless Chrome), SB_BIN (bridge executable, default
// target/debug/session-bridge), QA_CHROME, QA_SHOTS=all, QA_OUT, QA_LAN=1.
import { type Browser, driverName, launch } from "./framework/browser";
import { Bridge } from "./framework/bridge";
import { Ctx, Skip, type Action, type Domain, type Scenario } from "./framework/context";
import { Report } from "./framework/report";
import { Fatal } from "./framework/page";
import { domains } from "./domains";
import { scenarios } from "./scenarios";

type Test = { name: string; run: Action };

function catalog(): Test[] {
  const tests: Test[] = [];
  for (const domain of domains as Domain[]) {
    for (const [action, run] of Object.entries(domain.actions)) tests.push({ name: `${domain.name}.${action}`, run });
  }
  for (const scenario of scenarios as Scenario[]) tests.push({ name: scenario.name, run: scenario.run });
  return tests;
}

function choose(all: Test[], args: string[]): Test[] {
  if (args.length === 0) return all;
  const chosen: Test[] = [];
  for (const arg of args) {
    const hits = all.filter((t) => t.name === arg || t.name.startsWith(`${arg}.`));
    if (hits.length === 0) {
      console.error(`Unknown test "${arg}". Known:\n  ${all.map((t) => t.name).join("\n  ")}`);
      process.exit(2);
    }
    for (const hit of hits) if (!chosen.includes(hit)) chosen.push(hit);
  }
  return chosen;
}

async function main() {
  const args = process.argv.slice(2);
  if (args.includes("--list")) {
    console.log(catalog().map((t) => t.name).join("\n"));
    return 0;
  }
  const tests = choose(catalog(), args.filter((a) => !a.startsWith("--")));
  const report = new Report();
  const keepShots = process.env.QA_SHOTS === "all";
  let browser: Browser | undefined;
  let bridge: Bridge | undefined;
  const stopAll = async () => {
    await browser?.close().catch(() => {});
    await bridge?.stop().catch(() => {});
  };
  process.on("SIGINT", async () => {
    await stopAll();
    process.exit(130);
  });
  try {
    bridge = await Bridge.start("a");
    console.log(`driver: ${driverName()}${driverName() === "arc" ? " (one tab in the front Arc window, closed at the end)" : " (headless Chrome)"}`);
    browser = await launch();
    const page = await browser.page(await bridge.admin());
    // The tab opened on the admin page: check it answers (Arc: scripting
    // allowed) before any action.
    await page.waitFor(() => document.readyState === "complete", { message: "the first page load", timeout: 20_000 });
    for (const test of tests) {
      const ctx = new Ctx(page, bridge, (n) => report.shot(`${test.name}-${n}`), keepShots, browser);
      const started = Date.now();
      let error: string | undefined;
      let skipped = false;
      let fatal = false;
      let screenshot: string | undefined;
      try {
        await test.run(ctx);
      } catch (e) {
        skipped = e instanceof Skip;
        fatal = e instanceof Fatal;
        error = e instanceof Error ? e.message : String(e);
      }
      if (fatal) {
        report.add({ name: test.name, status: "fail", ms: Date.now() - started, steps: ctx.steps, error: `STOPPED: ${error}`, consoleErrors: [] });
        await ctx.cleanup().catch(() => {});
        break;
      }
      const consoleErrors = await page.takeErrors();
      if (!error && consoleErrors.length) error = `page console errors: ${consoleErrors.join(" | ")}`;
      if (error && !skipped) {
        screenshot = report.shot(`${test.name}-failure`);
        await page.screenshot(screenshot).catch(() => (screenshot = undefined));
      }
      report.add({
        name: test.name,
        status: skipped ? "skip" : error ? "fail" : "pass",
        ms: Date.now() - started,
        steps: ctx.steps,
        error,
        consoleErrors,
        screenshot,
        bridgeLog: error && !skipped ? bridge.log().slice(-2000) : undefined,
      });
      await ctx.cleanup().catch((e) => console.error(`cleanup: ${e}`));
    }
  } finally {
    await stopAll();
  }
  console.log(`\n${report.table()}`);
  return report.failed ? 1 : 0;
}

process.exit(await main());
