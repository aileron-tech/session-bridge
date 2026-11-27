---
name: verify-ui
description: Verify the session-bridge admin page (and the phone app) end to end with the qa/ framework - the whole suite, one domain, or one action such as cron.edit - against a separate test bridge with stand-in agents. Use after changing src/admin/*, src/phone/web/*, or any request the page sends, before reporting a UI change as done.
---

# Verify the admin UI

`qa/` drives the real admin page against a throwaway test bridge: its own
HOME and state under `/tmp/sbqa-*`, stand-in Claude/Codex/terminal/crontab/ssh,
never the installed service, the user's crontab or real agents. Every action
acts through the page and checks the result on the page and at the bridge
(what a stand-in session received, the crontab, the control API).

## Before running

1. Build the bridge you want to test: `cargo build` (or set `SB_BIN` to an
   executable built elsewhere, e.g. from a clean `git archive` of a commit).
2. Pick the driver:
   - Local default `QA_DRIVER=arc`: ONE new tab in the user's existing front
     Arc window, closed at the end (also on failure or Ctrl+C). Never another
     window or tab. In a multi-session setup, ask the coordinating session for
     "go" first and report when the tab is closed. Arc must allow JavaScript
     from Apple Events; if it does not, the run stops at the first call.
   - `QA_DRIVER=cdp` (the default when `CI` is set): headless Chrome with its
     own profile, no window. Needed for the phone app (`ctx.phonePage()`).
3. Phone access listens on the LAN and announces over mDNS: `phones.*` skip
   unless `QA_LAN=1`.

## Run

```sh
bun qa/run.ts --list                  # every test name
bun qa/run.ts                         # the whole suite
bun qa/run.ts cron                    # one domain
bun qa/run.ts cron.edit roles.assign  # single actions
bun qa/run.ts team-kickoff            # a scenario
```

Environment: `SB_BIN`, `QA_DRIVER=arc|cdp`, `QA_CHROME`, `QA_LAN=1`,
`QA_SHOTS=all` (a screenshot per step), `QA_OUT` (report folder).

The runner prints PASS/FAIL/SKIP per test, then a table and the path of
`report.json` (steps, errors, the bridge log tail and a failure screenshot per
failed test). It exits 1 on any failure or page console error.

## Reading a failure

- `timed out waiting for ...`: the selector or text it waited for, and the
  last value seen; open the failure screenshot from the report.
- `STOPPED: ...`: the browser backend refused or hung (Arc settings, a
  prompt); the run ended at once.
- A test that passes alone but fails in the suite usually depends on state
  another action left; actions must use `ctx.uid()` names and clean up.

## Adding an action

`qa/domains/<entity>.ts` exports `{ name, actions }`; each action is
`async (ctx) => { ... }` built from `ctx.step(name, fn)`:

- `ctx.open(tab)`, `ctx.nav(tab, key)`: a fresh page on a tab, a sidebar item by
  its `data-qa="nav:<tab>:<key>"` key (`new`, `all`, `role:<name>`, `project:<p>`).
- `ctx.page.click(...)` (a selector, or `{ scope, text, tag, within }`),
  `type`, `select`, `textContains`, `visible`, `hidden`, `waitFor`.
- `ctx.bridge.addClaude(name, project, { mute })`, `waitReceived(name, text)`,
  `crontab()`, `rpc(req)` (control socket), `ctx.pageOk(req)` (as the user).
- `ctx.dialog(prefix)`: a confirm() was asked (dialogs are answered "yes").
- `ctx.secondBridge()` for peers; `ctx.needsLan(...)`; throw `Skip` when an
  action cannot run here.

Prefer `data-qa` hooks where the page has them; otherwise ids and visible text.
Register new domains in `qa/domains/index.ts` and scenarios in
`qa/scenarios/index.ts`.
