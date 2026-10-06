import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { ConsoleAdapters } from "./index.ts";
import { createConsole } from "./index.ts";

async function receive(message: IncomingMessage, origin: string): Promise<Request> {
  const method = message.method ?? "GET";
  const chunks: Buffer[] = [];
  for await (const chunk of message) chunks.push(Buffer.from(chunk));
  const body = method === "GET" || method === "HEAD" ? undefined : Buffer.concat(chunks);
  const headers = new Headers();
  for (const [key, value] of Object.entries(message.headers)) {
    if (typeof value === "string") headers.set(key, value);
    else if (Array.isArray(value)) headers.set(key, value.join(", "));
  }
  return new Request(new URL(message.url ?? "/", origin), { method, headers, body });
}

async function send(reply: ServerResponse, response: Response): Promise<void> {
  reply.statusCode = response.status;
  response.headers.forEach((value, key) => reply.setHeader(key, value));
  reply.end(Buffer.from(await response.arrayBuffer()));
}

export function serveConsole(adapters: ConsoleAdapters, origin: string): Server {
  const app = createConsole(adapters);
  return createServer(async (message, reply) => {
    try {
      await send(reply, await app.fetch(await receive(message, origin)));
    } catch {
      await send(reply, Response.json({ error: { identifier: "ConsoleServerFailure" } }, { status: 500 }));
    }
  });
}
