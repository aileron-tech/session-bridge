// Phone access on the Peers tab (REQ-036). It listens on the LAN and
// announces sb-<name>.local over mDNS, so these actions run only with
// QA_LAN=1. The admin side is the real Phones pane; the phone side is a
// PhoneClient (the app's own requests) under any driver, and the phone app's
// page itself (app.pair) under the headless driver only: Arc does not trust
// the bridge's root, and it is never installed in the user's keychain.
import type { Json } from "../framework/bridge";
import type { Ctx, Domain } from "../framework/context";
import { PhoneClient, freePortPair } from "../framework/phone";

const PROJECT = "mono";

async function openPhones(ctx: Ctx) {
  await ctx.open("peers");
  await ctx.nav("peers", "phones");
  await ctx.page.visible("#phones-switch");
}

/** Phone access on through the page API, on free ports; returns the phones view. */
async function enable(ctx: Ctx) {
  const port = await freePortPair();
  const phones = (await ctx.pageOk({ method: "phone_access", enabled: true, port })).phones;
  ctx.expect(phones.listening, `phone access not listening: ${phones.problem ?? JSON.stringify(phones)}`);
  ctx.expect(phones.address, `no LAN address offered: ${JSON.stringify(phones)}`);
  return phones;
}

/** Unpairs every phone and turns phone access off. */
async function reset(ctx: Ctx) {
  const phones = (await ctx.bridge.page({ method: "phones" }).catch(() => null))?.phones;
  for (const device of phones?.devices ?? []) {
    await ctx.bridge.page({ method: "phone_unpair", device_id: device.id }).catch(() => {});
  }
  await ctx.bridge.page({ method: "phone_access", enabled: false }).catch(() => {});
}

/** The pane's "Pair a phone": returns the code it shows. */
async function offer(ctx: Ctx): Promise<string> {
  const before = (await ctx.page.text("#phones-offer-code").catch(() => "")).trim();
  await ctx.page.click("#phones-offer");
  await ctx.page.waitFor(
    (old: string) => {
      const code = document.getElementById("phones-offer-code")?.textContent?.trim() ?? "";
      return /^[A-Z2-7]{4}(-[A-Z2-7]{4}){3}$/.test(code) && code !== old;
    },
    { args: [before], message: "a pairing code in the pane" },
  );
  return (await ctx.page.text("#phones-offer-code")).trim();
}

/** The pane asks to confirm the phone with `key`; answers with `accept`. */
async function decide(ctx: Ctx, key: string, accept: boolean) {
  await ctx.page.visible("#phones-confirm", 15_000);
  await ctx.page.textContains("#phones-confirm-text", key);
  await ctx.page.click(accept ? "#phones-accept" : "#phones-decline");
}

/** A phone paired through the pane (accepted) and signed in. */
async function pairedPhone(ctx: Ctx, name = "QA phone"): Promise<PhoneClient> {
  const phones = await ctx.step("phone access on", () => enable(ctx));
  await ctx.step("open Phone access", () => openPhones(ctx));
  const code = await ctx.step("Pair a phone", () => offer(ctx));
  const phone = await ctx.step("the phone checks it reached this bridge", () => PhoneClient.create(phones.address, phones.port, phones.fingerprint));
  await ctx.step("the phone types the code", async () => {
    const reply = await phone.pair(code.toLowerCase(), name);
    ctx.expect(reply.status === 200, `pair: ${reply.status} ${JSON.stringify(reply.json)}`);
    ctx.expect(reply.json.fingerprint_short === phones.fingerprint_short, "the phone got another root fingerprint than the pane shows");
    await decide(ctx, reply.json.key, true);
  });
  await ctx.step("the device is listed and the phone signs in", async () => {
    await ctx.eventually("the phone learns it is paired", async () => (await phone.pairStatus()).state === "paired");
    await ctx.page.waitFor(() => document.querySelectorAll("#phones-devices-body tr").length === 1, { message: "one device row" });
    ctx.expect((await phone.signIn()) === 200, "the paired phone could not sign in");
  });
  return phone;
}

export const phones: Domain = {
  name: "phones",
  actions: {
    /** Turning phone access on starts the listener and shows step 1 with the IP link. */
    on: async (ctx) => {
      ctx.needsLan("Phone access");
      await ctx.step("open Phone access", () => openPhones(ctx));
      try {
        await ctx.step("turn it on, on free ports", async () => {
          await ctx.page.type("#phones-port", String(await freePortPair()));
          await ctx.page.click("#phones-toggle");
          await ctx.page.textContains("#phones-status", /Listening on https:\/\/\d+\.\d+\.\d+\.\d+:\d+/, 20_000);
          await ctx.page.visible("#phones-step1");
          await ctx.page.textContains("#phones-fingerprint", /[0-9A-F]{2}( [0-9A-F]{2}){7}/);
        });
      } finally {
        await reset(ctx);
      }
    },

    /** Turning it off stops listening and hides the steps. */
    off: async (ctx) => {
      ctx.needsLan("Phone access");
      try {
        await ctx.step("on through the page API", () => enable(ctx));
        await ctx.step("open Phone access", () => openPhones(ctx));
        await ctx.step("turn it off", async () => {
          await ctx.page.click("#phones-toggle");
          await ctx.page.hidden("#phones-step1");
          const phones = (await ctx.pageOk({ method: "phones" })).phones;
          ctx.expect(!phones.enabled && !phones.listening, `phones: ${JSON.stringify(phones)}`);
        });
      } finally {
        await reset(ctx);
      }
    },

    // Before the API actions, whose sign-ins share this machine's one
    // address and its pre-sign-in budget: the app waits out a 429 too, but
    // runs faster while the budget is fresh.
    /** The phone app itself (headless only): pair by typing the code, chat list, back to pairing after unpair. */
    "app.pair": async (ctx) => {
      ctx.needsLan("Phone access");
      const app = await ctx.phonePage();
      const name = ctx.uid("phone-app");
      try {
        await ctx.step("a session", () => ctx.bridge.addClaude(name, PROJECT));
        const phones = await ctx.step("phone access on", () => enable(ctx));
        await ctx.step("open Phone access", () => openPhones(ctx));
        const code = await ctx.step("Pair a phone", () => offer(ctx));
        await ctx.step("the app pairs with the typed code", async () => {
          await app.goto(`https://${phones.address}:${phones.port}/`);
          await app.waitFor(() => window.isSecureContext && !!crypto.subtle, { message: "WebCrypto in the app" });
          await app.visible("#pair");
          await app.type("#pair-code", code);
          await app.type("#pair-name", "QA app");
          // Like a user: when the bridge's sign-in limit answers "wait N s",
          // wait and press Pair again (a refused request does not use the code).
          const deadline = Date.now() + 90_000;
          for (;;) {
            // An earlier answer must not be read as this one's.
            await app.evaluate(() => {
              const error = document.getElementById("error");
              if (error) {
                error.textContent = "";
                error.hidden = true;
              }
              return true;
            });
            await app.click("#pair-form button");
            const outcome = await app.waitFor(
              () => (!document.getElementById("confirm")?.hidden ? "confirm" : /wait (\d+) s/.exec(document.getElementById("error")?.textContent ?? "")?.[1]),
              { message: "the confirm screen or a wait", timeout: 15_000 },
            );
            if (outcome === "confirm") break;
            ctx.expect(Date.now() < deadline, "the bridge kept refusing the app's pairing");
            await Bun.sleep(Number(outcome) * 1000 + 300);
          }
          ctx.expect((await app.text("#confirm-cert")).trim() === phones.fingerprint_short, "the app shows another root fingerprint");
          await decide(ctx, (await app.text("#confirm-key")).trim(), true);
        });
        await ctx.step("the app signs in and lists the session", async () => {
          // The app signs in on its next refresh (every 5 s), after any
          // Retry-After of the bridge's sign-in limit.
          await app.visible("#chats", 90_000);
          await app.textContains("#chat-list", name, 90_000);
          await app.textContains("#chat-list", `#${PROJECT}`);
        });
        await ctx.step("Unpair in the pane: the app returns to pairing", async () => {
          await ctx.page.click('#phones-devices-body [data-qa="phone-unpair"]');
          await ctx.dialog("Unpair");
          await app.click('#tabs [data-tab="cron"]');
          await app.visible("#pair", 30_000);
        });
      } finally {
        await reset(ctx);
      }
    },
    /** Declining in the pane saves nothing, and the phone learns it was declined. */
    "pair.decline": async (ctx) => {
      ctx.needsLan("Phone access");
      try {
        const phones = await ctx.step("phone access on", () => enable(ctx));
        await ctx.step("open Phone access", () => openPhones(ctx));
        const code = await ctx.step("Pair a phone", () => offer(ctx));
        const phone = await PhoneClient.create(phones.address, phones.port, phones.fingerprint);
        await ctx.step("the phone types the code; the pane declines", async () => {
          const reply = await phone.pair(code, "Declined phone");
          ctx.expect(reply.status === 200, `pair: ${reply.status} ${JSON.stringify(reply.json)}`);
          await decide(ctx, reply.json.key, false);
        });
        await ctx.step("nothing is saved; the phone is told", async () => {
          await ctx.eventually("the phone learns it was declined", async () => (await phone.pairStatus()).state === "declined");
          await ctx.page.visible("#phones-empty");
          ctx.expect((await ctx.pageOk({ method: "phones" })).phones.devices.length === 0, "a declined phone was saved");
        });
      } finally {
        await reset(ctx);
      }
    },

    /** Accepting in the pane lists the phone, which signs in; a wrong code uses the offer up. */
    "pair.accept": async (ctx) => {
      ctx.needsLan("Phone access");
      try {
        await pairedPhone(ctx);
        await ctx.step("a wrong code uses a new offer up", async () => {
          const phones = (await ctx.pageOk({ method: "phones" })).phones;
          const code = await offer(ctx);
          const other = await PhoneClient.create(phones.address, phones.port, phones.fingerprint);
          const wrong = (code[0] === "A" ? "B" : "A") + code.slice(1);
          ctx.expect((await other.pair(wrong, "Guess")).status === 403, "a wrong code was accepted");
          ctx.expect((await other.pair(code, "Guess")).status === 403, "the code still worked after a wrong guess");
        });
      } finally {
        await reset(ctx);
      }
    },

    /** A paired phone lists the sessions and reads the messages, without local paths. */
    chats: async (ctx) => {
      ctx.needsLan("Phone access");
      const name = ctx.uid("phone-chat");
      try {
        await ctx.step("a session", () => ctx.bridge.addClaude(name, PROJECT));
        const phone = await pairedPhone(ctx);
        await ctx.step("the phone lists sessions and messages", async () => {
          const listed = await phone.call({ method: "list", sender: null, project: null });
          ctx.expect(listed.status === 200, `list: ${listed.status}`);
          const session = listed.json.sessions.find((s: Json) => s.name === name);
          ctx.expect(session, `${name} not listed on the phone`);
          ctx.expect(session.cwd === "", `the phone sees a local path: ${session.cwd}`);
          const messages = await phone.call({ method: "messages" });
          ctx.expect(messages.status === 200 && Array.isArray(messages.json.messages), `messages: ${messages.status}`);
        });
        await ctx.step("laptop actions are refused", async () => {
          for (const request of [{ method: "phone_offer" }, { method: "network", enabled: false }, { method: "assign_role", session_id: "claude:x", role: "root" }]) {
            const reply = await phone.call(request);
            ctx.expect(reply.status === 403, `${request.method} from the phone: ${reply.status}`);
          }
        });
      } finally {
        await reset(ctx);
      }
    },

    /** A phone's message reaches the session as the user's words, never an approval; a repeat is sent once. */
    send: async (ctx) => {
      ctx.needsLan("Phone access");
      const name = ctx.uid("phone-send");
      try {
        const session = await ctx.step("a session", () => ctx.bridge.addClaude(name, PROJECT));
        const phone = await pairedPhone(ctx);
        const text = `From the phone ${ctx.uid("m")}`;
        await ctx.step("the phone sends twice with one client_id", async () => {
          const send = { method: "user_send", session_id: session.session_id, message: text, client_id: crypto.randomUUID() };
          const first = await phone.call(send);
          const again = await phone.call(send);
          ctx.expect(first.status === 200 && first.json.receipt, `send: ${first.status} ${JSON.stringify(first.json)}`);
          ctx.expect(again.json?.receipt?.message_id === first.json.receipt.message_id, "a repeated client_id was sent again");
        });
        await ctx.step("the session receives it once, as the user's words", async () => {
          const got = await ctx.bridge.waitReceived(name, text);
          ctx.expect(got.message.includes("it is not an approval"), `header missing: ${got.message.slice(0, 200)}`);
          await Bun.sleep(500);
          const copies = ctx.bridge.received(name).filter((r) => (r.message ?? "").includes(text)).length;
          ctx.expect(copies === 1, `received ${copies} copies`);
        });
      } finally {
        await reset(ctx);
      }
    },

    /** Unpairing in the pane ends the phone's session at once. */
    unpair: async (ctx) => {
      ctx.needsLan("Phone access");
      try {
        const phone = await pairedPhone(ctx);
        await ctx.step("Unpair in the pane", async () => {
          await ctx.page.click('#phones-devices-body [data-qa="phone-unpair"]');
          await ctx.dialog("Unpair");
          await ctx.page.visible("#phones-empty");
        });
        await ctx.step("the phone is shut out", async () => {
          const reply = await phone.call({ method: "messages" });
          ctx.expect(reply.status === 401, `call after unpair: ${reply.status}`);
          ctx.expect((await phone.signIn()) === 401, "an unpaired phone signed in again");
        });
      } finally {
        await reset(ctx);
      }
    },

  },
};
