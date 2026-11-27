// What waits for a session (REQ-032, REQ-035): a "mute" stand-in refuses its
// inbox, so the user's messages stay pending and are listed; Remove takes one back.
import type { Ctx, Domain } from "../framework/context";
import { openConversation } from "./chats";

async function pendingMessages(ctx: Ctx, count: number): Promise<string> {
  const name = ctx.uid("mute");
  const s = await ctx.bridge.addClaude(name, ctx.uid("waitp"), { mute: true });
  for (let i = 1; i <= count; i++) {
    const reply = await ctx.pageOk({ method: "user_send", session_id: s.session_id, message: `waiting message ${i}` });
    ctx.expect(reply.receipt.delivery.state === "pending", `expected pending, got ${reply.receipt.delivery.state}`);
  }
  return name;
}

/** The list is a collapsed <details> under the thread header: open it. */
export async function expandWaiting(ctx: Ctx) {
  await ctx.page.visible("#thread-waiting");
  await ctx.page.evaluate(() => {
    const details = document.querySelector("#thread-waiting details") as HTMLDetailsElement | null;
    if (details && !details.open) details.querySelector("summary")?.click();
  });
}

export const waiting: Domain = {
  name: "waiting",
  actions: {
    /** Pending messages are listed under the conversation, oldest first, with a warning. */
    list: async (ctx) => {
      const name = await ctx.step("two messages to a session that does not take them", () => pendingMessages(ctx, 2));
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      await ctx.step("the waiting list shows both", async () => {
        await ctx.page.visible("#thread-waiting");
        await ctx.page.textContains("#thread-waiting-summary", "2");
        await ctx.page.textContains("#thread-waiting-items", "waiting message 1");
        await ctx.page.textContains("#thread-waiting-items", "waiting message 2");
      });
      await ctx.step("the composer warns before sending more", async () => {
        await ctx.page.type("#composer-text", "one more");
        await ctx.page.textContains("#composer-warning", /queue/i);
      });
    },

    /** Remove asks, takes the message back, and the bridge marks it refused. */
    remove: async (ctx) => {
      const name = await ctx.step("one pending message", () => pendingMessages(ctx, 1));
      await ctx.step("open the conversation", () => openConversation(ctx, name));
      await ctx.step("Remove, confirmed", async () => {
        await expandWaiting(ctx);
        await ctx.page.click('li[data-qa^="waiting:"] [data-qa="waiting-remove"]');
        await ctx.dialog("Remove this message?");
      });
      await ctx.step("the list empties", () => ctx.page.hidden("#thread-waiting-items li"));
      await ctx.step("the bridge has nothing waiting for it", async () => {
        const id = (await ctx.bridge.waitSession(name)).session_id;
        await ctx.eventually("waiting cleared", async () => !(await ctx.pageOk({ method: "waiting" })).waiting[id]);
      });
    },
  },
};
