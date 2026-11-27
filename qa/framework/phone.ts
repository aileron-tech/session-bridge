// A phone, as the bridge sees one (REQ-036): the same requests and key as the
// phone app (src/phone/web/phone.js), without a browser, so the Arc driver
// can pair and use a phone too. The key is a WebCrypto P-256 key; sign-in
// signs "session-bridge phone v1\n<host>\n<nonce>" like the app.
//
// TLS: the listener is reached by the laptop's LAN IP under the bridge's own
// name-constrained root. Bun's TLS (BoringSSL) refuses roots with IP name
// constraints ("unsupported name constraint type"), so requests do not let
// TLS verify the chain. Instead, before any request, the client pins: the
// root from the download port must have the fingerprint the pane shows, and
// the certificate the listener serves must be signed by that root and name
// the address dialled. The full chain evaluation (name constraints included)
// is tested in tests/phone.rs with webpki and checked with macOS trustd.
import { X509Certificate } from "node:crypto";
import tls from "node:tls";
import type { Json } from "./bridge";

export type Reply = { status: number; json: Json };

function base64url(bytes: ArrayBuffer | Uint8Array): string {
  return Buffer.from(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)).toString("base64url");
}

export class PhoneClient {
  private token?: string;
  deviceId?: string;

  private constructor(
    /** The laptop's LAN address the phone uses. */
    readonly address: string,
    readonly port: number,
    private privateKey: CryptoKey,
    /** The uncompressed P-256 point, base64url: what the bridge pins. */
    readonly publicKey: string,
  ) {}

  /** A phone for the bridge at `address:port` whose root has `fingerprint` (the pane's, hex pairs). */
  static async create(address: string, port: number, fingerprint: string): Promise<PhoneClient> {
    await pin(address, port, fingerprint);
    const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
    const raw = await crypto.subtle.exportKey("raw", pair.publicKey);
    return new PhoneClient(address, port, pair.privateKey, base64url(raw));
  }

  get url(): string {
    return `https://${this.address}:${this.port}`;
  }

  /**
   * Before sign-in the bridge allows 10 requests per source address, then 10
   * a minute (REQ-036), and every action of a run comes from this machine's
   * address: a 429 is waited out (its Retry-After), for up to 90 s.
   */
  async post(path: string, body: Json = {}, auth = false): Promise<Reply> {
    const deadline = Date.now() + 90_000;
    for (;;) {
      const reply = await this.once(path, body, auth);
      if (reply.status !== 429 || Date.now() > deadline) return reply;
      await Bun.sleep(Number(reply.retryAfter ?? 2) * 1000 + 200);
    }
  }

  private async once(path: string, body: Json, auth: boolean): Promise<Reply & { retryAfter?: string }> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (auth && this.token) headers.Authorization = `Bearer ${this.token}`;
    const response = await fetch(`${this.url}${path}`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      tls: { rejectUnauthorized: false },
    } as RequestInit);
    const text = await response.text();
    let json: Json = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = text;
    }
    return { status: response.status, json, retryAfter: response.headers.get("retry-after") ?? undefined };
  }

  /** Types a pairing code, as the app's pairing form does. */
  pair(code: string, name: string): Promise<Reply> {
    return this.post("/api/pair", { code, name, public_key: this.publicKey });
  }

  /** Where the laptop's decision stands: waiting, paired, declined, expired. */
  async pairStatus(): Promise<Json> {
    const { json } = await this.post("/api/pair/status", { public_key: this.publicKey });
    if (json?.state === "paired") this.deviceId = json.device_id;
    return json;
  }

  /** Opens a session with a signed one-time challenge; returns the HTTP status. */
  async signIn(): Promise<number> {
    const challenge = await this.post("/api/challenge");
    if (challenge.status !== 200) return challenge.status;
    const nonce: string = challenge.json.nonce;
    const message = new TextEncoder().encode(`session-bridge phone v1\n${this.address}\n${nonce}`);
    const signature = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, this.privateKey, message);
    const reply = await this.post("/api/session", { device_id: this.deviceId, nonce, signature: base64url(signature) });
    this.token = reply.json?.token;
    return reply.status;
  }

  /** One control request through the phone allowlist. */
  call(request: Json): Promise<Reply> {
    return this.post("/api/call", request, true);
  }

  forget(): Promise<Reply> {
    return this.post("/api/forget", {}, true);
  }
}

/** Hex pairs, any separator, upper case: how fingerprints are compared. */
function hex(fingerprint: string): string {
  return fingerprint.replace(/[^0-9a-fA-F]/g, "").toUpperCase();
}

/** Throws unless the listener serves a certificate signed by the root with `fingerprint` for `address`. */
export async function pin(address: string, port: number, fingerprint: string): Promise<void> {
  const download = await fetch(`http://${address}:${port + 1}/bridge.crt`);
  if (!download.ok) throw new Error(`root download: HTTP ${download.status}`);
  const root = new X509Certificate(Buffer.from(await download.arrayBuffer()));
  if (hex(root.fingerprint256) !== hex(fingerprint)) {
    throw new Error(`the downloaded root ${root.fingerprint256} is not the one the pane shows (${fingerprint})`);
  }
  const served = await new Promise<Buffer>((resolve, reject) => {
    const socket = tls.connect({ host: address, port, rejectUnauthorized: false }, () => {
      const raw = socket.getPeerCertificate().raw;
      socket.end();
      raw ? resolve(raw) : reject(new Error("the listener sent no certificate"));
    });
    socket.on("error", reject);
    socket.setTimeout(10_000, () => socket.destroy(new Error("TLS connect timed out")));
  });
  const leaf = new X509Certificate(served);
  if (!leaf.verify(root.publicKey)) throw new Error("the served certificate is not signed by the bridge's root");
  if (!leaf.checkIP(address)) throw new Error(`the served certificate does not name ${address}`);
}

/** A free port whose next port is free too (HTTPS and the certificate download). */
export async function freePortPair(): Promise<number> {
  for (let tries = 0; tries < 50; tries++) {
    const first = Bun.listen({ hostname: "0.0.0.0", port: 0, socket: { data() {} } });
    const port = first.port;
    first.stop(true);
    try {
      const next = Bun.listen({ hostname: "0.0.0.0", port: port + 1, socket: { data() {} } });
      next.stop(true);
      if (port + 1 < 65535) return port;
    } catch {
      // taken: try another
    }
  }
  throw new Error("No free port pair for phone access");
}
