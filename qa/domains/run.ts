// Run tab: how sessions the user starts run (REQ-034). Saved settings show
// as the previewed command; a flag that would cut the bridge is refused.
import type { Ctx, Domain } from "../framework/context";

async function openRun(ctx: Ctx, { imported = true } = {}) {
  // The tab imports the shell aliases once on its own; settle that first so it
  // does not overwrite the fields while the action fills them.
  if (imported) await ctx.pageOk({ method: "import_aliases" });
  await ctx.open("run");
  await ctx.nav("run", "all");
  await ctx.page.visible("#run-form");
  // The form fills once the settings arrive; typing earlier would stop that fill.
  await ctx.page.textContains("#run-claude-preview", "claude ");
}

export const run: Domain = {
  name: "run",
  actions: {
    /** Approvals for Claude are saved and appear in the preview. */
    setApprovals: async (ctx) => {
      await ctx.step("open the Run tab", () => openRun(ctx));
      await ctx.step("choose Accept edits and save", async () => {
        await ctx.page.select("#run-claude-approvals", "accept-edits");
        await ctx.page.click("#run-save");
        await ctx.page.textContains("#run-note", "Saved.");
      });
      await ctx.step("preview and bridge agree", async () => {
        await ctx.page.textContains("#run-claude-preview", "--permission-mode acceptEdits");
        const settings = await ctx.pageOk({ method: "spawn_settings" });
        ctx.expect(settings.claude.approvals === "accept-edits", `saved approvals: ${settings.claude.approvals}`);
      });
      await ctx.step("back to Ask", async () => {
        await ctx.page.select("#run-claude-approvals", "ask");
        await ctx.page.click("#run-save");
        await ctx.page.textContains("#run-note", "Saved.");
      });
    },

    /** Allowed flags are saved; a flag that would cut the bridge is refused and nothing changes. */
    setFlags: async (ctx) => {
      await ctx.step("open the Run tab", () => openRun(ctx));
      await ctx.step("an allowed flag is saved", async () => {
        await ctx.page.type("#run-codex-flags", "--model gpt-5");
        await ctx.page.click("#run-save");
        await ctx.page.textContains("#run-note", "Saved.");
        await ctx.page.textContains("#run-codex-preview", "--model gpt-5");
      });
      await ctx.step("a refused flag is not saved", async () => {
        await ctx.page.type("#run-codex-flags", "--config=mcp_servers.session-bridge.enabled=false");
        await ctx.page.click("#run-save");
        await ctx.page.textContains("#run-note", "Not saved");
        const settings = await ctx.pageOk({ method: "spawn_settings" });
        ctx.expect(settings.codex.flags === "--model gpt-5", `flags changed to ${settings.codex.flags}`);
      });
    },

    /** Import from my shell fills approvals and flags from the zsh aliases. */
    import: async (ctx) => {
      await ctx.step("open the Run tab", () => openRun(ctx, { imported: false }));
      await ctx.step("Import from my shell", async () => {
        await ctx.page.click("#run-import");
        await ctx.page.textContains("#run-note", /Imported from your shell aliases|Filled in from your shell aliases/);
      });
      await ctx.step("the test home's aliases are in the fields", async () => {
        await ctx.page.waitFor(() => (document.getElementById("run-claude-flags") as HTMLInputElement).value.includes("--model opus"), { message: "claude flags imported" });
        const settings = await ctx.pageOk({ method: "spawn_settings" });
        ctx.expect(settings.imported === true, "not marked imported");
      });
    },
  },
};
