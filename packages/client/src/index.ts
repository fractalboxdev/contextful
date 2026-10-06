import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { ClientError } from "./browser.ts";
import type { Client } from "./browser.ts";

export { ClientError, createClient, listPackKeys } from "./browser.ts";
export type { Client, ClientShape, Listing } from "./browser.ts";

function findProjectRoot(cwd: string): string {
  let path = resolve(cwd);
  while (true) {
    if (existsSync(join(path, "contextful.toml"))) return path;
    const parent = dirname(path);
    if (parent === path) throw new ClientError("StoreSelectorAbsent", `no contextful.toml above ${cwd}`);
    path = parent;
  }
}

export type SpawnedOptions = { cwd: string; command: string; args?: string[]; token?: string };

export class SpawnedClient implements Client {
  readonly projectRoot: string;
  readonly command: string;
  private readonly args: string[];
  private readonly token: string;

  constructor(options: SpawnedOptions) {
    if (!options.token?.trim()) throw new ClientError("StdioCredentialMissing", "spawned engine requires a capability token");
    this.projectRoot = findProjectRoot(options.cwd);
    this.command = options.command;
    this.args = options.args ?? [];
    this.token = options.token;
  }

  async call(method: string, params: unknown = {}): Promise<unknown> {
    const child = spawn(this.command, ["mcp", ...this.args], {
      cwd: this.projectRoot,
      env: { ...process.env, CONTEXTFUL_TOKEN: this.token },
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (data: Buffer) => stdout.push(data));
    child.stderr.on("data", (data: Buffer) => stderr.push(data));
    child.stdin.end(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method, params })}\n`);
    const status = await new Promise<number | null>((resolveExit, reject) => {
      child.once("error", reject);
      child.once("close", resolveExit);
    });
    if (status !== 0) throw new ClientError("EngineProcessRefused", Buffer.concat(stderr).toString("utf8").trim());
    const line = Buffer.concat(stdout).toString("utf8").split("\n").find((text) => text.trim());
    if (!line) throw new ClientError("EngineProtocolRefused", "missing JSON-RPC response");
    const message = JSON.parse(line) as { result?: unknown; error?: { message?: string } };
    if (message.error) throw new ClientError("EngineProtocolRefused", message.error.message ?? "JSON-RPC error");
    if (!("result" in message)) throw new ClientError("EngineProtocolRefused", "missing JSON-RPC result");
    return message.result;
  }
}
