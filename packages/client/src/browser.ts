type Fetch = (request: Request) => Promise<Response>;
type Binding = { fetch: Fetch };

export type ClientShape =
  | { shape: "same-origin-proxy"; baseUrl: string; fetch?: Fetch }
  | { shape: "service-binding"; baseUrl: string; binding: Binding }
  | { shape: "engine-direct"; baseUrl: string; token: string; fetch?: Fetch };

export class ClientError extends Error {
  readonly identifier: string;
  constructor(identifier: string, message: string) {
    super(`${identifier}: ${message}`);
    this.identifier = identifier;
    this.name = identifier;
  }
}

export interface Client { call(method: string, params?: unknown): Promise<unknown> }

export function toolResult(result: unknown): unknown {
  if (typeof result !== "object" || result === null || !("isError" in result) || result.isError !== true) return result;
  const structured = "structuredContent" in result ? result.structuredContent : undefined;
  const error = typeof structured === "object" && structured !== null && "error" in structured ? structured.error : undefined;
  const detail = typeof error === "object" && error !== null ? error : undefined;
  const identifier = detail && "identifier" in detail && typeof detail.identifier === "string" ? detail.identifier : "EngineToolRefused";
  const content = "content" in result && Array.isArray(result.content) ? result.content : [];
  const text = content.find((item: unknown) => typeof item === "object" && item !== null && "type" in item && item.type === "text" && "text" in item && typeof item.text === "string") as { text: string } | undefined;
  const message = detail && "message" in detail && typeof detail.message === "string" ? detail.message : text?.text ?? "MCP tool refused";
  throw new ClientError(identifier, message);
}

function endpoint(shape: ClientShape): string {
  const url = new URL(shape.baseUrl);
  if (shape.shape === "same-origin-proxy") url.pathname = `${url.pathname.replace(/\/$/, "")}/mcp`;
  return url.toString();
}

export function createClient(shape: ClientShape): Client {
  const request = shape.shape === "service-binding" ? shape.binding.fetch.bind(shape.binding) : shape.fetch ?? fetch;
  return {
    async call(method, params = {}) {
      const headers = new Headers({ "content-type": "application/json", accept: "application/json" });
      if (shape.shape === "engine-direct") {
        if (!shape.token.trim()) throw new ClientError("EngineCredentialMissing", "engine-direct requires a scoped viewer token");
        headers.set("authorization", `Bearer ${shape.token}`);
      }
      const response = await request(new Request(endpoint(shape), { method: "POST", headers, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) }));
      if (!response.ok) throw new ClientError("EngineResponseRefused", `HTTP ${response.status}`);
      const message = await response.json() as { result?: unknown; error?: { code?: number; message?: string } };
      if (message.error) throw new ClientError("EngineProtocolRefused", message.error.message ?? String(message.error.code));
      if (!("result" in message)) throw new ClientError("EngineProtocolRefused", "missing JSON-RPC result");
      return toolResult(message.result);
    },
  };
}

export type Listing<T> = { entries: T[]; truncated: boolean; declined: number };

export function listPackKeys<T>(keys: Iterable<T>, serves: (key: T) => boolean): Listing<T> {
  const entries: T[] = [];
  let truncated = false;
  let declined = 0;
  for (const key of keys) {
    if (!serves(key)) declined += 1;
    else if (entries.length < 1000) entries.push(key);
    else truncated = true;
  }
  return { entries, truncated, declined };
}
