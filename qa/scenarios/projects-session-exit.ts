// Regression for "project items not changed in ui when a session exits"
// (02b6cd5): with a select of a project's start form focused, a session that
// ends leaves the card within a few seconds, and the user's choices stay.
import type { Scenario } from "../framework/context";

export const projectsSessionExit: Scenario = {
  name: "projects-session-exit",
  description: "focused start-form select; a session exits; its row leaves the card, the form keeps its choices",
  run: async (ctx) => {
    const project = ctx.uid("exit");
    const [stay, leave] = [ctx.uid("stay"), ctx.uid("leave")];
    const card = `article.project[data-project="${project}"]`;
    await ctx.step("two sessions in a project", async () => {
      await ctx.bridge.addClaude(stay, project);
      await ctx.bridge.addClaude(leave, project);
    });
    await ctx.step("open the project and choose Codex in its start form (focus stays on the select)", async () => {
      await ctx.open("projects");
      await ctx.nav("projects", `project:${project}`);
      await ctx.page.textContains(card, leave);
      await ctx.page.select(`${card} form.project__spawn [data-field="agent"]`, "codex");
      await ctx.page.evaluate((sel: string) => (document.querySelector(sel) as HTMLElement).focus(), `${card} form.project__spawn [data-field="agent"]`);
    });
    await ctx.step("a session exits", async () => {
      ctx.bridge.endSession(leave);
      await ctx.bridge.waitGone(leave);
    });
    await ctx.step("its row leaves the card within 8 s, the other stays", async () => {
      await ctx.page.waitFor((sel: string, name: string) => !document.querySelector(sel)?.textContent?.includes(name), {
        args: [card, leave],
        timeout: 8_000,
        message: `${leave} removed from the card`,
      });
      await ctx.page.textContains(card, stay);
    });
    await ctx.step("the form kept the choice and the focus", async () => {
      const kept = await ctx.page.evaluate((sel: string) => {
        const select = document.querySelector(sel) as HTMLSelectElement;
        return { value: select.value, focused: document.activeElement === select };
      }, `${card} form.project__spawn [data-field="agent"]`);
      ctx.expect(kept.value === "codex", `agent choice lost: ${kept.value}`);
      ctx.expect(kept.focused, "focus left the select");
    });
  },
};
