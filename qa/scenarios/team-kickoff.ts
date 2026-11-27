// A team's first hour: roles, sessions started from the channel, a kickoff
// with the team table, a message taken back before it was read, and closing.
import type { Scenario } from "../framework/context";
import { openConversation } from "../domains/chats";
import { expandWaiting } from "../domains/waiting";

export const teamKickoff: Scenario = {
  name: "team-kickoff",
  description: "create roles -> start sessions -> kickoff in channel -> remove a waiting message -> close",
  run: async (ctx) => {
    const project = ctx.uid("team");
    const lead = ctx.uid("lead");
    const [backend, frontend] = [ctx.uid("backend"), ctx.uid("frontend")];
    await ctx.step("the lead is already working in the project", () => ctx.bridge.addClaude(lead, project));

    await ctx.step("create two roles on the Roles tab", async () => {
      for (const role of [backend, frontend]) {
        await ctx.open("roles");
        await ctx.nav("roles", "New role");
        await ctx.page.type("#role-name", role);
        await ctx.page.type("#role-prompt", `You own the ${role} work.`);
        await ctx.page.click("#role-save");
        await ctx.nav("roles", "All roles");
        await ctx.page.textContains("#roles-body", role);
      }
    });

    const started: string[] = [];
    await ctx.step("start one session per role from the channel", async () => {
      for (const role of [backend, frontend]) {
        await openConversation(ctx, `#${project}`);
        await ctx.page.click("#spawn-open");
        await ctx.page.select("#spawn-role", role);
        await ctx.page.click("#spawn-start");
        await ctx.page.textContains("#composer-hint", "Starting");
        const name = (await ctx.page.text("#composer-hint")).match(/Starting (\S+)/)![1];
        await ctx.bridge.waitSession(name);
        started.push(name);
      }
    });

    await ctx.step("kickoff in the channel reaches everyone with the team table", async () => {
      await openConversation(ctx, `#${project}`);
      await ctx.page.click("#composer-kickoff");
      const text = await ctx.page.value("#composer-text");
      await ctx.page.type("#composer-text", `@channel ${text}`);
      await ctx.page.click({ scope: "#composer", text: "Send", tag: "button" });
      await ctx.page.textContains("#composer-hint", "Posted with the team table to 3 sessions");
      for (const name of [lead, ...started]) {
        const got = await ctx.bridge.waitReceived(name, "Kick off");
        ctx.expect(got.message.includes("Bridge-generated team snapshot"), `${name} got no team table`);
      }
    });

    await ctx.step("a message to a busy session is taken back", async () => {
      const busy = ctx.uid("busy");
      const s = await ctx.bridge.addClaude(busy, project, { mute: true });
      await ctx.pageOk({ method: "user_send", session_id: s.session_id, message: "please also update the docs" });
      await openConversation(ctx, busy);
      await ctx.page.textContains("#thread-waiting-items", "please also update the docs");
      await expandWaiting(ctx);
      await ctx.page.click({ scope: "#thread-waiting-items", text: "Remove", tag: "button" });
      await ctx.page.hidden("#thread-waiting-items li");
      ctx.bridge.endSession(busy);
    });

    await ctx.step("close the started sessions from the Projects tab", async () => {
      for (const name of started) {
        await ctx.open("projects");
        await ctx.nav("projects", `#${project}`);
        await ctx.page.click({ scope: `article.project[data-project="${project}"]`, text: name, tag: "tr", within: "button" });
        await ctx.page.textContains("#projects-note", `Closed ${name}`);
        await ctx.bridge.waitGone(name);
      }
    });
  },
};
