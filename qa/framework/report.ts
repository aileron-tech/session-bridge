// Results of a run: one row per action or scenario, a JSON report and the
// screenshots of failures (and of every step with QA_SHOTS=all) in a temp
// folder that is kept after the run.
import { mkdirSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export type Result = {
  name: string;
  status: "pass" | "fail" | "skip";
  ms: number;
  steps: string[];
  error?: string;
  consoleErrors: string[];
  screenshot?: string;
  bridgeLog?: string;
};

export class Report {
  readonly dir = mkdtempSync(join(process.env.QA_OUT ?? tmpdir(), "sbqa-report-"));
  readonly results: Result[] = [];

  constructor() {
    mkdirSync(join(this.dir, "shots"), { recursive: true });
  }

  shot(name: string): string {
    return join(this.dir, "shots", `${name.replace(/[^a-zA-Z0-9.-]+/g, "_")}.png`);
  }

  add(result: Result) {
    this.results.push(result);
    const mark = result.status.toUpperCase();
    console.log(`${mark}  ${result.name}  (${(result.ms / 1000).toFixed(1)} s)${result.error ? `\n      ${result.error.split("\n")[0]}` : ""}`);
  }

  get failed(): number {
    return this.results.filter((r) => r.status === "fail").length;
  }

  write(): string {
    const file = join(this.dir, "report.json");
    writeFileSync(file, JSON.stringify({ at: new Date().toISOString(), results: this.results }, null, 2));
    return file;
  }

  table(): string {
    const width = Math.max(10, ...this.results.map((r) => r.name.length));
    const lines = [`${"test".padEnd(width)}  result  time    note`, `${"-".repeat(width)}  ------  ------  ----`];
    for (const r of this.results) {
      const note = r.status !== "pass" ? (r.error ?? "").split("\n")[0].slice(0, 100) : "";
      lines.push(`${r.name.padEnd(width)}  ${r.status.toUpperCase().padEnd(6)}  ${`${(r.ms / 1000).toFixed(1)}s`.padEnd(6)}  ${note}`);
    }
    const passed = this.results.filter((r) => r.status === "pass").length;
    const skipped = this.results.filter((r) => r.status === "skip").length;
    lines.push("", `${passed} passed, ${this.failed} failed, ${skipped} skipped, ${this.results.length} total. Report: ${this.write()}`);
    return lines.join("\n");
  }
}
