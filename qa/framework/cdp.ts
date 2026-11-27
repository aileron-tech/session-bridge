// Minimal Chrome DevTools Protocol client over bun's WebSocket: commands
// with ids, events by method, sessions for attached targets (flatten mode).

type Pending = { resolve: (value: any) => void; reject: (error: Error) => void; method: string };
type Listener = (params: any, sessionId?: string) => void;

export class Cdp {
  private socket: WebSocket;
  private next = 1;
  private pending = new Map<number, Pending>();
  private listeners = new Map<string, Set<Listener>>();
  private closed = false;

  private constructor(socket: WebSocket) {
    this.socket = socket;
    socket.addEventListener("message", (event) => this.receive(String(event.data)));
    socket.addEventListener("close", () => {
      this.closed = true;
      for (const [, p] of this.pending) p.reject(new Error(`CDP closed during ${p.method}`));
      this.pending.clear();
    });
  }

  static connect(url: string, timeoutMs = 10_000): Promise<Cdp> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      const timer = setTimeout(() => reject(new Error(`CDP connect timed out: ${url}`)), timeoutMs);
      socket.addEventListener("open", () => {
        clearTimeout(timer);
        resolve(new Cdp(socket));
      });
      socket.addEventListener("error", () => {
        clearTimeout(timer);
        reject(new Error(`CDP connect failed: ${url}`));
      });
    });
  }

  private receive(text: string) {
    const message = JSON.parse(text);
    if (typeof message.id === "number") {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      else pending.resolve(message.result);
      return;
    }
    for (const listener of this.listeners.get(message.method) ?? []) listener(message.params, message.sessionId);
  }

  send<T = any>(method: string, params: object = {}, sessionId?: string, timeoutMs = 30_000): Promise<T> {
    if (this.closed) return Promise.reject(new Error(`CDP closed before ${method}`));
    const id = this.next++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, {
        method,
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
      this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  on(method: string, listener: Listener): () => void {
    const set = this.listeners.get(method) ?? new Set();
    set.add(listener);
    this.listeners.set(method, set);
    return () => set.delete(listener);
  }

  close() {
    if (!this.closed) this.socket.close();
  }
}
