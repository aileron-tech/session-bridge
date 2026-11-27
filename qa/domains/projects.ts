// Projects tab: one card per project directory with its sessions, master
// choice, start form and Close buttons (REQ-030).
import type { Ctx, Domain } from "../framework/context";

const card = (project: string) => `article.project[data-project="${project}"]`;

async function openProject(ctx: Ctx, project: string) {
  await ctx.open("projects");
  await ctx.nav("projects", `#${project}`);
  await ctx.page.visible(card(project));
}

export const projects: Domain = {
  name: "projects",
  actions: {
    /** A project's card lists its sessions; the sidebar names the project. */
    open: async (ctx) => {
      const project = ctx.uid("proj");
      const [a, b] = [ctx.uid("pa"), ctx.uid("pb")];
      await ctx.step("two sessions in one project", async () => {
        await ctx.bridge.addClaude(a, project);
        await ctx.bridge.addClaude(b, project);
      });
      await ctx.step("open the project", () => openProject(ctx, project));
      await ctx.step("the card lists both", async () => {
        await ctx.page.textContains(card(project), a);
        await ctx.page.textContains(card(project), b);
      });
    },

    /** Choosing a master is saved by session and shown with a badge. */
    setMaster: async (ctx) => {
      const project = ctx.uid("pm");
      const [lead, dev] = [ctx.uid("lead"), ctx.uid("dev")];
      await ctx.step("two sessions", async () => {
        await ctx.bridge.addClaude(lead, project);
        await ctx.bridge.addClaude(dev, project);
      });
      await ctx.step("open the project", () => openProject(ctx, project));
      await ctx.step("choose the master", async () => {
        await ctx.page.select(`${card(project)} .project__master select`, dev);
        await ctx.page.textContains("#projects-note", `${dev} is the master of #${project}`);
      });
      await ctx.step("the bridge has it as chosen", () =>
        ctx.eventually("master chosen", async () => {
          const view = (await ctx.pageOk({ method: "projects" })).projects.find((p: any) => p.project === project);
          return view?.master === dev && view.master_chosen;
        }),
      );
    },

    /** Start session from the card: a stand-in starts with the role and note. */
    start: async (ctx) => {
      const project = ctx.uid("ps");
      const role = ctx.uid("helper");
      await ctx.step("a project and a role", async () => {
        await ctx.bridge.addClaude(ctx.uid("first"), project);
        await ctx.pageOk({ method: "save_role", name: role, prompt: `You help as ${role}.`, previous: null });
      });
      await ctx.step("open the project", () => openProject(ctx, project));
      const name = await ctx.step("start from the card", async () => {
        const form = `${card(project)} form.project__spawn`;
        await ctx.page.select(`${form} [data-field="agent"]`, "claude");
        await ctx.page.select(`${form} [data-field="role"]`, role);
        await ctx.page.type(`${form} [data-field="note"]`, "Pair with the lead.");
        await ctx.page.click({ scope: form, text: "Start session", tag: "button" });
        await ctx.page.textContains("#projects-note", "Starting");
        return (await ctx.page.text("#projects-note")).match(/Starting (\S+)/)![1];
      });
      await ctx.step("it appears in the card, briefed", async () => {
        await ctx.page.textContains(card(project), name, 20_000);
        const first = await ctx.bridge.waitReceived(name, `You help as ${role}.`);
        ctx.expect(first.first_prompt.includes("Pair with the lead."), "note missing from the first prompt");
      });
    },

    /** Close in a session's row asks and ends it; the row goes. */
    close: async (ctx) => {
      const project = ctx.uid("pc");
      const [stay, go] = [ctx.uid("stay"), ctx.uid("go")];
      await ctx.step("two sessions", async () => {
        await ctx.bridge.addClaude(stay, project);
        await ctx.bridge.addClaude(go, project);
      });
      await ctx.step("open the project", () => openProject(ctx, project));
      await ctx.step("Close, confirmed", async () => {
        await ctx.page.click({ scope: card(project), text: go, tag: "tr", within: "button" });
        await ctx.page.textContains("#projects-note", `Closed ${go}`);
        await ctx.dialog(`Close ${go}?`);
      });
      await ctx.step("the row goes, the other stays", async () => {
        await ctx.bridge.waitGone(go);
        await ctx.page.waitFor((sel: string, name: string) => !document.querySelector(sel)?.textContent?.includes(name), { args: [card(project), go], timeout: 15_000, message: "row removed" });
        await ctx.page.textContains(card(project), stay);
      });
    },
  },
};
