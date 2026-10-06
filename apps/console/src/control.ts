import type { ConsoleAdapters } from "./index.ts";
import { createHash, createHmac, randomBytes } from "node:crypto";

type ControlStore = { id: string; endpoint: string };
type ControlOptions = { stores: ControlStore[]; capability?: string; attestationSecret?: string; fetcher?: typeof fetch };

function unavailable(): never {
  throw new Error("ConsoleAdapterUnavailable");
}

function selected(stores: ControlStore[], id: string | null): ControlStore {
  if (id !== null) {
    const store = stores.find((candidate) => candidate.id === id);
    if (store) return store;
    throw new Error("ConsoleStoreUnknown");
  }
  if (stores.length === 1) return stores[0];
  throw new Error("ConsoleStoreSelectionRequired");
}

function controlDocument(document: unknown, editing: boolean): { store: string | null; body: { expected: number; document?: string; nonce?: string } } {
  if (document === null || typeof document !== "object" || Array.isArray(document)) throw new Error("ConsoleRequestMalformed");
  const value = document as Record<string, unknown>;
  if (value.store !== undefined && typeof value.store !== "string") throw new Error("ConsoleRequestMalformed");
  if (!Number.isSafeInteger(value.expected) || (value.expected as number) < 1) throw new Error("ConsoleRequestMalformed");
  if (editing && typeof value.document !== "string") throw new Error("ConsoleRequestMalformed");
  if (!editing && (typeof value.nonce !== "string" || !/^[0-9a-f]{48}$/.test(value.nonce))) throw new Error("ConsoleRequestMalformed");
  const keys = Object.keys(value);
  if (keys.some((key) => key !== "store" && key !== "expected" && (key !== "document" || !editing) && (key !== "nonce" || editing))) throw new Error("ConsoleRequestMalformed");
  return { store: typeof value.store === "string" ? value.store : null,
    body: editing ? { expected: value.expected as number, document: value.document as string } : { expected: value.expected as number, nonce: value.nonce as string } };
}

export function createLiveControl({ stores, capability, attestationSecret, fetcher = fetch }: ControlOptions): ConsoleAdapters["control"] {
  async function call(store: ControlStore, path: string, token: string, body?: object, subject?: string): Promise<unknown> {
    const serialized = body ? JSON.stringify(body) : undefined;
    const headers: Record<string, string> = { Authorization: `Bearer ${token}`, ...(body ? { "Content-Type": "application/json" } : {}) };
    if (body && subject && attestationSecret) {
      const timestamp = Math.floor(Date.now() / 1000).toString();
      const nonce = randomBytes(16).toString("hex");
      const digest = createHash("sha256").update(serialized!).digest("hex");
      const signature = createHmac("sha256", attestationSecret).update(`POST\n${path}\n${digest}\n${subject}\n${timestamp}\n${nonce}`).digest("hex");
      Object.assign(headers, { "X-Contextful-Operator": subject, "X-Contextful-Operator-Time": timestamp, "X-Contextful-Operator-Nonce": nonce, "X-Contextful-Operator-Signature": signature });
    }
    const response = await fetcher(new URL(path, store.endpoint), {
      method: body ? "POST" : "GET",
      headers,
      ...(serialized ? { body: serialized } : {}),
    });
    if (!response.ok) throw new Error(`ConsoleControlRefused:${response.status}`);
    return response.json();
  }
  return {
    workflows: async (_operator, id) => capability ? call(selected(stores, id), "/control/workflows", capability) : unavailable(),
    record: async (_operator, id) => capability ? call(selected(stores, id), "/control/record", capability) : unavailable(),
    edit: async (document, token, operator) => {
      if (!capability || token !== capability) return unavailable();
      const input = controlDocument(document, true);
      return call(selected(stores, input.store), "/control/edit", token, input.body, operator.subject);
    },
    apply: async (document, token, operator) => {
      if (!capability || token !== capability) return unavailable();
      const input = controlDocument(document, false);
      return call(selected(stores, input.store), "/control/apply", token, input.body, operator.subject);
    },
  };
}
