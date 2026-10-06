import type { ConsoleAdapters } from "./index.ts";

type ControlStore = { id: string; endpoint: string };
type ControlOptions = { stores: ControlStore[]; capability?: string; fetcher?: typeof fetch };

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

function applyDocument(document: unknown): { store: string | null; body: { id?: string } } {
  if (document === null || typeof document !== "object" || Array.isArray(document)) throw new Error("ConsoleRequestMalformed");
  const value = document as Record<string, unknown>;
  if (value.store !== undefined && typeof value.store !== "string") throw new Error("ConsoleRequestMalformed");
  if (value.id !== undefined && typeof value.id !== "string") throw new Error("ConsoleRequestMalformed");
  return { store: typeof value.store === "string" ? value.store : null, body: typeof value.id === "string" ? { id: value.id } : {} };
}

export function createLiveControl({ stores, capability, fetcher = fetch }: ControlOptions): ConsoleAdapters["control"] {
  async function call(store: ControlStore, path: string, token: string, body?: object): Promise<unknown> {
    const response = await fetcher(new URL(path, store.endpoint), {
      method: body ? "POST" : "GET",
      headers: { Authorization: `Bearer ${token}`, ...(body ? { "Content-Type": "application/json" } : {}) },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    if (!response.ok) throw new Error(`ConsoleControlRefused:${response.status}`);
    return response.json();
  }
  return {
    workflows: async (_operator, id) => capability ? call(selected(stores, id), "/control/workflows", capability) : unavailable(),
    record: async () => unavailable(),
    edit: async () => unavailable(),
    apply: async (document, token) => {
      if (!capability || token !== capability) return unavailable();
      const input = applyDocument(document);
      return call(selected(stores, input.store), "/control/apply", token, input.body);
    },
  };
}
