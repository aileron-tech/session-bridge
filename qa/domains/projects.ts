// Projects tab: one card per project directory with its sessions, master
// choice, start form and Close buttons (REQ-030).
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { Ctx, Domain } from "../framework/context";

const card = (project: string) => `article.project[data-project="${project}"]`;

async function openProject(ctx: Ctx, project: string) {
  await ctx.open("projects");
  await ctx.nav("projects", `project:${project}`);
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
        await ctx.page.click(`${card(project)} [data-qa="project-start"]`);
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
        const id = (await ctx.bridge.waitSession(go)).session_id;
        await ctx.page.click(`tr[data-qa="project-session:${id}"] [data-qa="project-close"]`);
        await ctx.page.textContains("#projects-note", `Closed ${go}`);
        await ctx.dialog(`Close ${go}?`);
      });
      await ctx.step("the row goes, the other stays", async () => {
        await ctx.bridge.waitGone(go);
        await ctx.page.waitFor((sel: string, name: string) => !document.querySelector(sel)?.textContent?.includes(name), { args: [card(project), go], timeout: 15_000, message: "row removed" });
        await ctx.page.textContains(card(project), stay);
      });
    },

    /** REQ-040: the star button stars and unstars a project; starred projects
     * come first, then live ones, then by name; stars.json changes only when
     * a star does; the control socket cannot star. */
    star: async (ctx) => {
      // Names sort ended < live < starred-live, so the order shows the rule, not the alphabet.
      const ended = ctx.uid("pa-ended");
      const live = ctx.uid("pb-live");
      const starred = ctx.uid("pz-star");
      const [e, l, s] = [ctx.uid("e"), ctx.uid("l"), ctx.uid("s")];
      const order = () =>
        ctx.page.evaluate((names: string[]) =>
          [...document.querySelectorAll("#project-cards article.project")].map((c) => (c as HTMLElement).dataset.project).filter((p) => names.includes(p!)),
        [ended, live, starred]);
      const pressed = (project: string) =>
        ctx.page.evaluate((sel: string) => document.querySelector(sel)?.getAttribute("aria-pressed"), `${card(project)} [data-qa="project-star"]`);
      const starsFile = join(ctx.bridge.state, "stars.json");
      // The folder as the bridge reports it (/tmp is /private/tmp on macOS).
      const dirOf = async (project: string): Promise<string> =>
        (await ctx.pageOk({ method: "projects" })).projects.find((p: any) => p.project === project).dir;
      const stars = (): string[] => (existsSync(starsFile) ? JSON.parse(readFileSync(starsFile, "utf8")) : []);
      await ctx.step("an ended project and two live ones", async () => {
        await ctx.bridge.addClaude(e, ended);
        await ctx.bridge.addClaude(l, live);
        await ctx.bridge.addClaude(s, starred);
        ctx.bridge.endSession(e);
        await ctx.bridge.waitGone(e);
      });
      await ctx.step("open Projects: live before ended, none starred", async () => {
        await ctx.open("projects");
        await ctx.page.visible(card(ended));
        await ctx.page.waitFor((sel: string) => !document.querySelector(sel)?.querySelector("tr"), { args: [card(ended)], message: "ended card without rows" });
        const now = await order();
        ctx.expect(JSON.stringify(now) === JSON.stringify([live, starred, ended]), `order ${JSON.stringify(now)}`);
        ctx.expect((await pressed(starred)) === "false", "star pressed before a click");
      });
      await ctx.step("star the last live project: it moves first, saved", async () => {
        await ctx.page.click(`${card(starred)} [data-qa="project-star"]`);
        await ctx.page.waitFor((sel: string) => document.querySelector(sel)?.getAttribute("aria-pressed") === "true", { args: [`${card(starred)} [data-qa="project-star"]`], message: "aria-pressed true" });
        await ctx.page.textContains(`[data-qa="nav:projects:project:${starred}"]`, `\u2605 #${starred}`);
        const now = await order();
        ctx.expect(JSON.stringify(now) === JSON.stringify([starred, live, ended]), `order ${JSON.stringify(now)}`);
        ctx.expect(stars().includes(await dirOf(starred)), `stars.json ${JSON.stringify(stars())}`);
      });
      await ctx.step("star the ended project: starred live, starred ended, live", async () => {
        await ctx.page.click(`${card(ended)} [data-qa="project-star"]`);
        await ctx.page.waitFor((sel: string) => document.querySelector(sel)?.getAttribute("aria-pressed") === "true", { args: [`${card(ended)} [data-qa="project-star"]`], message: "aria-pressed true" });
        const now = await order();
        ctx.expect(JSON.stringify(now) === JSON.stringify([starred, ended, live]), `order ${JSON.stringify(now)}`);
      });
      await ctx.step("starring again does not rewrite stars.json", async () => {
        const before = statSync(starsFile).ino;
        await ctx.pageOk({ method: "project_star", dir: await dirOf(ended), starred: true });
        await ctx.pageOk({ method: "project_star", dir: await dirOf(live), starred: false });
        ctx.expect(statSync(starsFile).ino === before, "stars.json rewritten without a change");
      });
      await ctx.step("a relative folder and the control socket are refused", async () => {
        const relative = await ctx.bridge.page({ method: "project_star", dir: "work/x", starred: true });
        ctx.expect(relative.result === "error", `relative folder: ${JSON.stringify(relative)}`);
        const liveDir = await dirOf(live);
        const socket = await ctx.bridge.rpc({ method: "project_star", dir: liveDir, starred: true });
        ctx.expect(socket.result === "error", `control socket: ${JSON.stringify(socket)}`);
        ctx.expect(!stars().includes(liveDir), "the control socket starred");
      });
      await ctx.step("unstar both: back to live, then ended", async () => {
        await ctx.page.click(`${card(starred)} [data-qa="project-star"]`);
        await ctx.page.waitFor((sel: string) => document.querySelector(sel)?.getAttribute("aria-pressed") === "false", { args: [`${card(starred)} [data-qa="project-star"]`], message: "aria-pressed false" });
        await ctx.page.click(`${card(ended)} [data-qa="project-star"]`);
        await ctx.page.waitFor((sel: string) => document.querySelector(sel)?.getAttribute("aria-pressed") === "false", { args: [`${card(ended)} [data-qa="project-star"]`], message: "aria-pressed false" });
        const now = await order();
        ctx.expect(JSON.stringify(now) === JSON.stringify([live, starred, ended]), `order ${JSON.stringify(now)}`);
        const mine = await Promise.all([ended, live, starred].map(dirOf));
        const left = stars().filter((d) => mine.includes(d));
        ctx.expect(left.length === 0, `stars.json keeps ${JSON.stringify(left)}`);
      });
    },

    /** REQ-038: a project stays after its last session ends, ready for a new one. */
    remembered: async (ctx) => {
      const project = ctx.uid("pr");
      const only = ctx.uid("only");
      await ctx.step("one session", () => ctx.bridge.addClaude(only, project));
      await ctx.step("open the project", () => openProject(ctx, project));
      await ctx.step("the session ends", async () => {
        ctx.bridge.endSession(only);
        await ctx.bridge.waitGone(only);
      });
      await ctx.step("the card stays without the session, its start form kept", async () => {
        await ctx.page.waitFor((sel: string, name: string) => !document.querySelector(sel)?.textContent?.includes(name), { args: [card(project), only], timeout: 15_000, message: "row removed" });
        await ctx.page.visible(`${card(project)} form.project__spawn`);
        const view = (await ctx.pageOk({ method: "projects" })).projects.find((p: any) => p.project === project);
        ctx.expect(view && view.sessions.length === 0, `project not remembered: ${JSON.stringify(view)}`);
      });
    },
  },
};
