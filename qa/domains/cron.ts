// Cron tab: scheduled channel posts kept as lines of the (stand-in) crontab (REQ-025).
import type { Ctx, Domain } from "../framework/context";

async function createSchedule(ctx: Ctx, project: string, message: string) {
  await ctx.open("cron");
  await ctx.nav("cron", "new");
  await ctx.page.type("#schedule-cron", "0 9 * * 1-5");
  await ctx.page.select("#schedule-project", project);
  await ctx.page.type("#schedule-message", message);
  await ctx.page.click("#schedule-save");
  await ctx.nav("cron", "all");
  await ctx.page.textContains("#schedules-body", message);
}

/** A button of the schedule whose message is `message`: run, edit or delete. */
async function rowButton(ctx: Ctx, message: string, action: "run" | "edit" | "delete") {
  const id = (await scheduleOf(ctx, message)).id;
  await ctx.page.click(`tr[data-qa="schedule:${id}"] [data-qa="schedule-${action}"]`);
}

async function scheduleOf(ctx: Ctx, message: string) {
  const reply = await ctx.pageOk({ method: "schedules" });
  return reply.schedules.find((s: any) => s.message === message);
}

export const cron: Domain = {
  name: "cron",
  actions: {
    /** A new schedule is listed and installed as one crontab line, without the message text. */
    create: async (ctx) => {
      const project = ctx.uid("cron");
      const message = `standup ${ctx.uid("c")}`;
      await ctx.step("a session in the channel", () => ctx.bridge.addClaude(ctx.uid("cs"), project));
      await ctx.step("add the schedule", () => createSchedule(ctx, project, message));
      await ctx.step("installed in crontab by id, message not in it", async () => {
        const entry = await scheduleOf(ctx, message);
        ctx.expect(entry?.installed, "schedule not installed");
        const table = ctx.bridge.crontab();
        ctx.expect(table.includes(`scheduled ${entry.id}`), "no crontab line for the schedule");
        ctx.expect(!table.includes(message), "the message text leaked into the crontab");
      });
    },

    /** Edit changes the message in place; the crontab keeps one line. */
    edit: async (ctx) => {
      const project = ctx.uid("cred");
      const before = `before ${ctx.uid("e")}`;
      const after = `after ${ctx.uid("e")}`;
      await ctx.step("a schedule", async () => {
        await ctx.bridge.addClaude(ctx.uid("ce"), project);
        await createSchedule(ctx, project, before);
      });
      await ctx.step("Edit and save", async () => {
        await rowButton(ctx, before, "edit");
        await ctx.page.visible("#schedule-form");
        await ctx.page.type("#schedule-message", after);
        await ctx.page.click("#schedule-save");
      });
      await ctx.step("the table and the bridge show the new text", async () => {
        await ctx.nav("cron", "all");
        await ctx.page.textContains("#schedules-body", after);
        const entry = await ctx.eventually("edited", () => scheduleOf(ctx, after));
        const lines = ctx.bridge.crontab().split("\n").filter((l) => l.includes(entry.id));
        ctx.expect(lines.length === 1, `expected one crontab line, got ${lines.length}`);
      });
    },

    /** Run now posts at once and reports who got it. */
    runNow: async (ctx) => {
      const project = ctx.uid("crun");
      const target = ctx.uid("cr");
      const message = `@channel run ${ctx.uid("r")}`;
      await ctx.step("a schedule", async () => {
        await ctx.bridge.addClaude(target, project);
        await createSchedule(ctx, project, message);
      });
      await ctx.step("Run now", async () => {
        await rowButton(ctx, message, "run");
        await ctx.page.textContains("#schedules-note", "Posted to 1 session");
      });
      await ctx.step("the session got it as a scheduled post", async () => {
        const got = await ctx.bridge.waitReceived(target, message.replace("@channel ", ""));
        ctx.expect(/Scheduled message from the user/.test(got.message), "not marked as a scheduled post");
      });
    },

    /** Delete asks, removes the row and its crontab line. */
    delete: async (ctx) => {
      const project = ctx.uid("cdel");
      const message = `gone ${ctx.uid("d")}`;
      await ctx.step("a schedule", async () => {
        await ctx.bridge.addClaude(ctx.uid("cd"), project);
        await createSchedule(ctx, project, message);
      });
      const id = (await scheduleOf(ctx, message)).id;
      await ctx.step("Delete, confirmed", async () => {
        await rowButton(ctx, message, "delete");
        await ctx.dialog("Delete the schedule");
      });
      await ctx.step("row and crontab line are gone", async () => {
        await ctx.page.waitFor((text: string) => !document.querySelector("#schedules-body")?.textContent?.includes(text), { args: [message], message: "row removed" });
        ctx.expect(!ctx.bridge.crontab().includes(id), "crontab line still there");
      });
    },
  },
};
