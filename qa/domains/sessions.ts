// Starting and closing sessions from the Chats tab (REQ-028, REQ-034): the
// stand-in terminal runs the bridge's launch, which becomes the stand-in Claude.
import type { Domain } from "../framework/context";
import { openConversation } from "./chats";

export const sessions: Domain = {
  name: "sessions",
  actions: {
    /** New session with a role and the user's note; it appears, named and briefed. */
    start: async (ctx) => {
      const project = ctx.uid("start");
      const role = ctx.uid("worker");
      await ctx.step("a project with a live session, and a role", async () => {
        await ctx.bridge.addClaude(ctx.uid("lead"), project);
        await ctx.pageOk({ method: "save_role", name: role, prompt: `You are the ${role}.`, previous: null });
      });
      await ctx.step("open the channel", () => openConversation(ctx, `#${project}`));
      await ctx.step("fill New session", async () => {
        await ctx.page.click("#spawn-open");
        await ctx.page.visible("#spawn-form");
        await ctx.page.select("#spawn-agent", "claude");
        await ctx.page.select("#spawn-role", role);
        await ctx.page.type("#spawn-note", "Start with the login flow.");
      });
      await ctx.step("start", async () => {
        await ctx.page.click("#spawn-start");
        await ctx.page.textContains("#composer-hint", "Starting");
      });
      const name = await ctx.step("it appears, named {short}-{role}-1", async () => {
        const hint = await ctx.page.text("#composer-hint");
        const started = hint.match(/Starting (\S+)/)?.[1];
        ctx.expect(started?.endsWith(`-${role}-1`), `unexpected session name in "${hint}"`);
        await ctx.bridge.waitSession(started!);
        return started!;
      });
      await ctx.step("its first prompt carries the role and the note, never interactive questions", async () => {
        const first = await ctx.bridge.waitReceived(name, `You are the ${role}.`);
        ctx.expect(first.first_prompt.includes("Start with the login flow."), "the note is missing from the first prompt");
        const args = JSON.parse(await Bun.file(`${ctx.bridge.dir}/received/${name}.args`).text());
        ctx.expect(args.includes("--disallowedTools") && args.includes("AskUserQuestion"), `fixed flags missing: ${args}`);
        ctx.expect(!args.includes("--dangerously-skip-permissions"), "a started session got bypass flags by default");
      });
    },

    /** Close ends the session after confirming; it leaves the list. */
    close: async (ctx) => {
      const name = ctx.uid("bye");
      await ctx.step("a live session", () => ctx.bridge.addClaude(name, ctx.uid("closep")));
      await ctx.step("open it", () => openConversation(ctx, name));
      await ctx.step("Close, confirmed", async () => {
        await ctx.page.click("#close-session");
        await ctx.page.textContains("#composer-hint", `Closed ${name}`);
        await ctx.dialog(`Close ${name}?`);
      });
      await ctx.step("the bridge no longer lists it", () => ctx.bridge.waitGone(name));
    },
  },
};
