// Usage on the Peers tab (REQ-029): this laptop's CPU, memory and disk, and
// each local session's process.
import type { Domain } from "../framework/context";

export const usage: Domain = {
  name: "usage",
  actions: {
    view: async (ctx) => {
      const name = ctx.uid("use");
      await ctx.step("a live session", () => ctx.bridge.addClaude(name, ctx.uid("usep")));
      await ctx.step("open This bridge", async () => {
        await ctx.open("peers");
        await ctx.nav("peers", "self");
      });
      await ctx.step("host usage is shown", () => ctx.page.textContains("#self-usage", /CPU \d+%, memory .* of .* GiB, disk/, 15_000));
      await ctx.step("the session's process is listed", async () => {
        await ctx.page.visible("#usage-sessions", 15_000);
        await ctx.page.textContains("#usage-sessions-body", name, 15_000);
        await ctx.page.textContains("#usage-sessions-body", "CPU");
      });
    },
  },
};
