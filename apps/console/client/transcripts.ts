// Saved chats carry answer rows. They restore only under the scope digest the stores
// route returned for the verified operator and reading session that wrote them, and only
// for a store that route still lists; anything else is discarded, never rebound.

type Saved = { store: string };

export function saveSaved<T extends Saved>(scope: string, sessions: T[]): string {
  return JSON.stringify({ scope, sessions });
}

export function restoreSaved<T extends Saved>(raw: string | null, scope: string, stores: ReadonlyArray<{ id: string }>): T[] {
  if (!raw || !scope) return [];
  let value: unknown;
  try { value = JSON.parse(raw); } catch { return []; }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [];
  const saved = value as { scope?: unknown; sessions?: unknown };
  if (saved.scope !== scope || !Array.isArray(saved.sessions)) return [];
  return saved.sessions.filter((session): session is T =>
    typeof session === "object" && session !== null && stores.some((store) => store.id === (session as Saved).store));
}
