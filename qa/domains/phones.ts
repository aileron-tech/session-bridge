// Phone access on the Peers tab (REQ-036). It listens on the LAN and
// announces sb-<name>.local over mDNS, so these actions run only with
// QA_LAN=1. Pairing (decline/accept) and unpairing are to come from
// session-bridge-mobile-ios-1's phone e2e scripts.
import type { Ctx, Domain } from "../framework/context";
import { Skip } from "../framework/context";

async function openPhones(ctx: Ctx) {
  await ctx.open("peers");
  await ctx.nav("peers", "Phone access");
  await ctx.page.visible("#phones-switch");
}

export const phones: Domain = {
  name: "phones",
  actions: {
    /** Turning phone access on starts the listener and shows step 1. */
    on: async (ctx) => {
      ctx.needsLan("Phone access");
      await ctx.step("open Phone access", () => openPhones(ctx));
      try {
        await ctx.step("turn it on", async () => {
          await ctx.page.click("#phones-toggle");
          await ctx.page.textContains("#phones-status", /Listening on https:\/\//, 20_000);
          await ctx.page.visible("#phones-step1");
          await ctx.page.textContains("#phones-fingerprint", /[0-9A-F]{2}( [0-9A-F]{2}){7}/);
        });
      } finally {
        await ctx.bridge.page({ method: "phone_access", enabled: false }).catch(() => {});
      }
    },

    /** Turning it off stops listening and hides the steps. */
    off: async (ctx) => {
      ctx.needsLan("Phone access");
      await ctx.step("on through the page API", () => ctx.pageOk({ method: "phone_access", enabled: true }));
      await ctx.step("open Phone access", () => openPhones(ctx));
      await ctx.step("turn it off", async () => {
        await ctx.page.click("#phones-toggle");
        await ctx.page.hidden("#phones-step1");
        const phones = (await ctx.pageOk({ method: "phones" })).phones;
        ctx.expect(!phones.enabled && !phones.listening, `phones: ${JSON.stringify(phones)}`);
      });
    },

    pair: async () => {
      throw new Skip("phone pairing (decline/accept, unpair) is being ported from mobile-ios's e2e scripts");
    },
  },
};
