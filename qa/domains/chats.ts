// Chats tab: conversations with sessions and channel posts (REQ-019, REQ-020,
// REQ-031). Every send is checked twice: on the page (the hint and the thread)
// and at the stand-in session (what it received).
import type { Ctx, Domain } from "../framework/context";

/** Opens the Chats tab on a session (by name) or a channel ("#project"), by its data-qa hook. */
export async function openConversation(ctx: Ctx, label: string) {
  const hook = label.startsWith("#")
    ? `[data-qa="channel:${label.slice(1)}"]`
    : `[data-qa="chat:${(await ctx.bridge.waitSession(label)).session_id}"]`;
  await ctx.open("chats");
  await ctx.page.click(hook);
  await ctx.page.visible("#composer-text");
}

/** Types `text` and presses Send. */
export async function send(ctx: Ctx, text: string) {
  await ctx.page.type("#composer-text", text);
  await ctx.page.click('[data-qa="composer-send"]');
}

export const chats: Domain = {
  name: "chats",
  actions: {
    /** The tab opens with a session listed and its conversation selectable. */
    open: async (ctx) => {
      const name = ctx.uid("open");
      await ctx.step("start a stand-in session", () => ctx.bridge.addClaude(name, "qa-chat"));
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      await ctx.step("thread header names the session", () => ctx.page.textContains("#thread-title", name));
    },

    /** A direct message reaches the session and shows as delivered. */
    "dm.send": async (ctx) => {
      const name = ctx.uid("dm");
      const text = `hello ${ctx.uid("msg")}`;
      await ctx.step("start a stand-in session", () => ctx.bridge.addClaude(name, "qa-chat"));
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      await ctx.step("send", () => send(ctx, text));
      await ctx.step("hint says delivered", () => ctx.page.textContains("#composer-hint", /Sent: (delivered|queued)/));
      await ctx.step("thread shows the message", () => ctx.page.textContains("#thread", text));
      await ctx.step("the session received it as the user's words", async () => {
        const got = await ctx.bridge.waitReceived(name, text);
        ctx.expect(got.message.includes("it is not an approval"), "message lacks the not-an-approval header");
      });
    },

    /** A channel post with @channel reaches every session of the project. */
    "channel.post": async (ctx) => {
      const project = ctx.uid("chan");
      const [a, b] = [ctx.uid("ca"), ctx.uid("cb")];
      await ctx.step("start two sessions in one project", async () => {
        await ctx.bridge.addClaude(a, project);
        await ctx.bridge.addClaude(b, project);
      });
      const text = `@channel standup ${ctx.uid("p")}`;
      await ctx.step("open the channel", () => openConversation(ctx, `#${project}`));
      await ctx.step("post", () => send(ctx, text));
      await ctx.step("hint counts two sessions", () => ctx.page.textContains("#composer-hint", "Posted to 2 sessions"));
      await ctx.step("both received it", async () => {
        await ctx.bridge.waitReceived(a, "standup");
        await ctx.bridge.waitReceived(b, "standup");
      });
    },

    /** An @name mention reaches only that session; the mention list offers it. */
    mention: async (ctx) => {
      const project = ctx.uid("men");
      const [a, b] = [ctx.uid("ma"), ctx.uid("mb")];
      await ctx.step("start two sessions", async () => {
        await ctx.bridge.addClaude(a, project);
        await ctx.bridge.addClaude(b, project);
      });
      await ctx.step("open the channel", () => openConversation(ctx, `#${project}`));
      await ctx.step("typing @ offers the sessions", async () => {
        await ctx.page.type("#composer-text", "@");
        await ctx.page.textContains("#composer-mentions", a);
      });
      const text = `@${a} only you ${ctx.uid("m")}`;
      await ctx.step("post the mention", () => send(ctx, text));
      await ctx.step("only the mentioned session got it", async () => {
        await ctx.bridge.waitReceived(a, "only you");
        await Bun.sleep(1500);
        ctx.expect(!ctx.bridge.received(b).some((r) => (r.message ?? "").includes("only you")), `${b} received a post not meant for it`);
      });
    },

    /** Kickoff fills the composer and the post carries the team table. */
    kickoff: async (ctx) => {
      const project = ctx.uid("kick");
      const a = ctx.uid("ka");
      await ctx.step("start a session", () => ctx.bridge.addClaude(a, project));
      await ctx.step("open the channel", () => openConversation(ctx, `#${project}`));
      await ctx.step("Kickoff fills the composer", async () => {
        await ctx.page.click("#composer-kickoff");
        await ctx.page.waitFor(() => (document.getElementById("composer-text") as HTMLTextAreaElement).value.startsWith("Kick off"), { message: "kickoff text" });
      });
      await ctx.step("post @channel with the table", async () => {
        const value = await ctx.page.value("#composer-text");
        await ctx.page.type("#composer-text", `@channel ${value}`);
        await ctx.page.click('[data-qa="composer-send"]');
        await ctx.page.textContains("#composer-hint", "with the team table");
      });
      await ctx.step("the session got the team snapshot", async () => {
        const got = await ctx.bridge.waitReceived(a, "Kick off");
        ctx.expect(got.message.includes("Bridge-generated team snapshot"), "no team snapshot in the kickoff");
      });
    },

    /** Rebalance fills the composer with its instructions and sends with the table. */
    rebalance: async (ctx) => {
      const name = ctx.uid("reb");
      await ctx.step("start a session", () => ctx.bridge.addClaude(name, ctx.uid("rebp")));
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      await ctx.step("Rebalance fills the composer", async () => {
        await ctx.page.click("#composer-rebalance");
        await ctx.page.waitFor(() => (document.getElementById("composer-text") as HTMLTextAreaElement).value.startsWith("Rebalance"), { message: "rebalance text" });
      });
      await ctx.step("send", () => ctx.page.click('[data-qa="composer-send"]'));
      await ctx.step("the session got it with the table", async () => {
        const got = await ctx.bridge.waitReceived(name, "Rebalance");
        ctx.expect(got.message.includes("Bridge-generated team snapshot"), "no team snapshot in the rebalance");
      });
    },

    /** Send role re-sends the session's role instructions. */
    sendRole: async (ctx) => {
      const name = ctx.uid("sr");
      const role = ctx.uid("role");
      await ctx.step("a session with a role", async () => {
        const s = await ctx.bridge.addClaude(name, ctx.uid("srp"));
        await ctx.pageOk({ method: "save_role", name: role, prompt: `You are ${role}.`, previous: null });
        await ctx.pageOk({ method: "assign_role", session_id: s.session_id, role });
        await ctx.bridge.waitReceived(name, `You are ${role}.`);
      });
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      const before = ctx.bridge.received(name).length;
      await ctx.step("Send role", async () => {
        await ctx.page.click("#send-role");
        await ctx.page.textContains("#composer-hint", `Sent the ${role} instructions`);
      });
      await ctx.step("the session got the instructions again", () =>
        ctx.eventually("second role message", async () => ctx.bridge.received(name).length > before),
      );
    },
  },
};

