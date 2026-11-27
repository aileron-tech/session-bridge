// Roles tab: role texts, assignment (sent to the session as the user's
// message), and deletion (REQ-026, REQ-028: root stays built in).
import type { Ctx, Domain } from "../framework/context";

async function createRole(ctx: Ctx, name: string, prompt: string) {
  await ctx.open("roles");
  await ctx.nav("roles", "new");
  await ctx.page.type("#role-name", name);
  await ctx.page.type("#role-prompt", prompt);
  await ctx.page.click("#role-save");
  await ctx.nav("roles", "all");
  await ctx.page.textContains("#roles-body", name);
}

const roleButton = (name: string, action: "edit" | "delete") => `tr[data-qa="role:${name}"] [data-qa="role-${action}"]`;

async function roleOf(ctx: Ctx, name: string) {
  return (await ctx.pageOk({ method: "roles" })).roles.find((r: any) => r.name === name);
}

export const roles: Domain = {
  name: "roles",
  actions: {
    /** A new role is listed (before root) with its text. */
    create: async (ctx) => {
      const name = ctx.uid("tester");
      await ctx.step("add the role", () => createRole(ctx, name, `You test ${name}.`));
      await ctx.step("the bridge has it, root stays last", async () => {
        const book = await ctx.pageOk({ method: "roles" });
        const names = book.roles.map((r: any) => r.name);
        ctx.expect(names.includes(name), "role not saved");
        ctx.expect(names[names.length - 1] === "root", `root is not last: ${names}`);
      });
    },

    /** Editing from the sidebar changes the text. */
    edit: async (ctx) => {
      const name = ctx.uid("editor");
      await ctx.step("a role", () => createRole(ctx, name, "First text."));
      await ctx.step("open it from the sidebar and change the text", async () => {
        await ctx.nav("roles", `role:${name}`);
        await ctx.page.visible("#role-form");
        await ctx.page.type("#role-prompt", "Second text.");
        await ctx.page.click("#role-save");
      });
      await ctx.step("saved", () => ctx.eventually("new text", async () => (await roleOf(ctx, name))?.prompt === "Second text."));
    },

    /** Edit in the table opens the role's form (the same as its sidebar item). */
    editFromTable: async (ctx) => {
      const name = ctx.uid("tbl");
      await ctx.step("a role", () => createRole(ctx, name, "Table text."));
      await ctx.step("Edit in its row shows the form", async () => {
        await ctx.page.click(roleButton(name, "edit"));
        await ctx.page.visible("#role-form", 5_000);
        ctx.expect((await ctx.page.value("#role-name")) === name, "the form is not filled with the role");
      });
    },

    /** Assigning sends the instructions to the session and shows the holder. */
    assign: async (ctx) => {
      const role = ctx.uid("rev");
      const session = ctx.uid("as");
      await ctx.step("a role and a live session", async () => {
        await ctx.bridge.addClaude(session, ctx.uid("asp"));
        await createRole(ctx, role, `You review as ${role}.`);
      });
      await ctx.step("Assign a role", async () => {
        await ctx.nav("roles", "assign");
        await ctx.page.select("#assign-session", session);
        await ctx.page.select("#assign-role", role);
        await ctx.page.click('[data-qa="assign-submit"]');
        await ctx.page.textContains("#roles-note", `Sent the ${role} instructions to ${session}`);
      });
      await ctx.step("the session got them; the table names the holder", async () => {
        await ctx.bridge.waitReceived(session, `You review as ${role}.`);
        await ctx.nav("roles", "all");
        await ctx.page.textContains("#roles-body", session);
      });
    },

    /** Delete asks and removes the role; root cannot be deleted. */
    delete: async (ctx) => {
      const name = ctx.uid("temp");
      await ctx.step("a role", () => createRole(ctx, name, "Short-lived."));
      await ctx.step("Delete, confirmed", async () => {
        await ctx.page.click(roleButton(name, "delete"));
        await ctx.dialog(`Delete the role ${name}?`);
        await ctx.eventually("deleted", async () => !(await roleOf(ctx, name)));
      });
      await ctx.step("root refuses deletion", async () => {
        await ctx.page.click(roleButton("root", "delete"));
        await ctx.page.textContains("#error", /built in/i);
        ctx.expect(await roleOf(ctx, "root"), "root was deleted");
      });
    },
  },
};
