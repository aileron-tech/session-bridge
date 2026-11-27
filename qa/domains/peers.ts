// Peers tab: this bridge's networking, pairing a second laptop, and remote
// hosts over (stand-in) SSH (REQ-017, REQ-027). Networking is kept
// loopback-only (local_only, set once over the control socket and kept by
// the page's requests), so nothing is announced on the real network.
import type { Bridge } from "../framework/bridge";
import type { Ctx, Domain } from "../framework/context";

async function localOnly(bridge: Bridge, name: string, on: boolean): Promise<number | null> {
  const reply = await bridge.rpc({ method: "network", enabled: on, port: 0, name, local_only: true });
  if (reply.result === "error") throw new Error(`network ${on ? "on" : "off"}: ${reply.message}`);
  return reply.lan?.network?.port ?? null;
}

async function openPeers(ctx: Ctx, pane: string) {
  await ctx.open("peers");
  await ctx.nav("peers", pane);
}

export const peers: Domain = {
  name: "peers",
  actions: {
    /** Turning networking on shows the listening port. */
    networkOn: async (ctx) => {
      await ctx.step("loopback-only and off", () => localOnly(ctx.bridge, "qa-a", false));
      await ctx.step("open This bridge", () => openPeers(ctx, "This bridge"));
      try {
        await ctx.step("Turn networking on", async () => {
          await ctx.page.textContains("#network-toggle", "Turn networking on");
          await ctx.page.click("#network-toggle");
          await ctx.page.textContains("#self", /networking on, port \d+/);
          await ctx.page.textContains("#network-toggle", "Turn networking off");
        });
        await ctx.step("still loopback-only", async () => {
          const lan = (await ctx.bridge.rpc({ method: "lan" })).lan;
          ctx.expect(lan.network.enabled && lan.network.local_only !== false, `network: ${JSON.stringify(lan.network)}`);
        });
      } finally {
        await localOnly(ctx.bridge, "qa-a", false);
      }
    },

    /** Turning networking off stops listening. */
    networkOff: async (ctx) => {
      await ctx.step("loopback-only and on", () => localOnly(ctx.bridge, "qa-a", true));
      await ctx.step("open This bridge", () => openPeers(ctx, "This bridge"));
      await ctx.step("Turn networking off", async () => {
        await ctx.page.textContains("#network-toggle", "Turn networking off");
        await ctx.page.click("#network-toggle");
        await ctx.page.textContains("#self", "networking off");
      });
      await ctx.step("the bridge stopped listening", async () => {
        const lan = (await ctx.bridge.rpc({ method: "lan" })).lan;
        ctx.expect(!lan.network.enabled && !lan.network.port, `network: ${JSON.stringify(lan.network)}`);
      });
    },

    /** Pairing a second bridge by address with the code it shows. */
    pairLaptop: async (ctx) => {
      const b = await ctx.step("a second bridge listening on loopback", () => ctx.secondBridge());
      const port = await localOnly(b, "qa-b", true);
      await localOnly(ctx.bridge, "qa-a", true);
      try {
        await ctx.step("open Laptops on this network", () => openPeers(ctx, "Laptops on this network"));
        await ctx.step("Pair by address", async () => {
          await ctx.page.type("#pair-address", `127.0.0.1:${port}`);
          await ctx.page.click({ scope: "#pair", text: "Pair", tag: "button" });
          await ctx.page.visible("#code-input");
        });
        const code = await ctx.step("the second bridge shows a code", () =>
          ctx.eventually("incoming code", async () => (await b.rpc({ method: "lan" })).lan.pairing?.incoming?.code),
        );
        await ctx.step("type the code", async () => {
          await ctx.page.type("#code-input", code);
          await ctx.page.click({ scope: "#code", text: "Pair", tag: "button" });
        });
        await ctx.step("both list each other as paired", async () => {
          await ctx.page.textContains("#hosts-body", "qa-b");
          await ctx.page.textContains("#hosts-body", "paired");
          await ctx.eventually("b paired with a", async () => (await b.rpc({ method: "lan" })).lan.hosts.some((h: any) => h.host === "qa-a" && h.paired));
        });
      } finally {
        await ctx.bridge.rpc({ method: "unpair", host: "qa-b" }).catch(() => {});
        await b.rpc({ method: "unpair", host: "qa-a" }).catch(() => {});
        await localOnly(ctx.bridge, "qa-a", false);
      }
    },

    /** A remote host over SSH connects through the (stand-in) tunnel, then is removed. */
    remoteAddRemove: async (ctx) => {
      const b = await ctx.step("a remote bridge on loopback", () => ctx.secondBridge());
      const port = await localOnly(b, "qa-b", true);
      await localOnly(ctx.bridge, "qa-a", true);
      const host = "qa-remote";
      try {
        await ctx.step("open Remote hosts over SSH", () => openPeers(ctx, "Remote hosts over SSH"));
        await ctx.step("Add remote host", async () => {
          await ctx.page.type("#remote-host", host);
          await ctx.page.type("#remote-port", String(port));
          await ctx.page.click({ scope: "#remote-form", text: "Add remote host", tag: "button" });
          await ctx.page.textContains("#remotes-note", `Connecting to ${host}`);
        });
        await ctx.step("the tunnel connects", async () => {
          await ctx.eventually("connected", async () => (await ctx.bridge.rpc({ method: "remotes" })).remotes?.some((r: any) => r.host === host && r.state === "connected"), 30_000);
          await ctx.page.textContains("#remotes-body", "connected", 15_000);
        });
        await ctx.step("Remove, confirmed", async () => {
          // The row also has a Pair button: click Remove by its label.
          await ctx.page.click({ scope: "#remotes-body", text: "Remove", tag: "button" });
          await ctx.dialog(`Remove ${host}?`);
          await ctx.eventually("removed", async () => !(await ctx.bridge.rpc({ method: "remotes" })).remotes?.some((r: any) => r.host === host));
        });
      } finally {
        await ctx.bridge.rpc({ method: "remove_remote", host }).catch(() => {});
        await localOnly(ctx.bridge, "qa-a", false);
      }
    },
  },
};
