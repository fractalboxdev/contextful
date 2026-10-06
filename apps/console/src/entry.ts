import { createPublicKey } from "node:crypto";
import { pathToFileURL } from "node:url";
import { isAbsolute, resolve } from "node:path";
import { registryFromEnv } from "../../gateway/src/index.ts";
import { createLiveAnswer } from "./answer_live.ts";
import type { ConsoleAdapters, Identity } from "./index.ts";
import { serveConsole } from "./server.ts";

type HostedAdapters = Pick<ConsoleAdapters, "turn" | "control"> &
  Partial<Pick<ConsoleAdapters, "read" | "brief" | "briefBudgetMs" | "redactView">>;
type AdapterFactory = (input: { stores: ConsoleAdapters["stores"]; env: NodeJS.ProcessEnv }) => Promise<HostedAdapters>;

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

async function keysAt(url: string): Promise<ReadonlyMap<string, ReturnType<typeof createPublicKey>>> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`identity key route returned ${response.status}`);
  const document: unknown = await response.json();
  if (!document || typeof document !== "object" || !("keys" in document) || !Array.isArray(document.keys)) {
    throw new Error("identity key route returned no key set");
  }
  const keys = new Map<string, ReturnType<typeof createPublicKey>>();
  for (const value of document.keys) {
    if (!value || typeof value !== "object" || value.kty !== "RSA" || value.alg !== "RS256" || typeof value.kid !== "string") continue;
    keys.set(value.kid, createPublicKey({ key: value, format: "jwk" }));
  }
  if (keys.size === 0) throw new Error("identity key route returned no RS256 key");
  return keys;
}

async function identity(): Promise<Identity> {
  if (process.env.CONTEXTFUL_IDENTITY === "cognito") {
    const keys = await keysAt(required("CONTEXTFUL_COGNITO_JWKS_URL"));
    return {
      kind: "cognito",
      sessionSecret: required("CONTEXTFUL_COGNITO_SESSION_SECRET"),
      queryGroup: required("CONTEXTFUL_COGNITO_QUERY_GROUP"),
      adminGroup: required("CONTEXTFUL_COGNITO_ADMIN_GROUP"),
      issuer: required("CONTEXTFUL_COGNITO_ISSUER"),
      clientId: required("CONTEXTFUL_COGNITO_CLIENT_ID"),
      authorizeUrl: required("CONTEXTFUL_COGNITO_AUTHORIZE_URL"),
      tokenUrl: required("CONTEXTFUL_COGNITO_TOKEN_URL"),
      redirectUri: required("CONTEXTFUL_COGNITO_REDIRECT_URI"),
      keys,
    };
  }
  return {
    kind: "access",
    issuer: required("CONTEXTFUL_ACCESS_ISSUER"),
    queryAudience: required("CONTEXTFUL_QUERY_ACCESS_AUDIENCE"),
    adminAudience: required("CONTEXTFUL_ADMIN_ACCESS_AUDIENCE"),
    keys: await keysAt(required("CONTEXTFUL_ACCESS_JWKS_URL")),
  };
}

function unavailable(): never {
  throw new Error("ConsoleAdapterUnavailable");
}

function unavailableAdapters(): Pick<ConsoleAdapters, "turn" | "control"> {
  return {
    turn: async () => unavailable(),
    control: {
      workflows: async () => unavailable(),
      record: async () => unavailable(),
      edit: async () => unavailable(),
      apply: async () => unavailable(),
    },
  };
}

async function main(): Promise<void> {
  const addressFlag = process.argv.indexOf("--http");
  if (addressFlag < 0 || !process.argv[addressFlag + 1]) throw new Error("--http <host:port> is required");
  const address = process.argv[addressFlag + 1];
  const separator = address.lastIndexOf(":");
  const host = address.slice(0, separator);
  const port = Number(address.slice(separator + 1));
  if (!host || !Number.isInteger(port) || port < 0 || port > 65535) throw new Error("invalid --http address");
  const registry = registryFromEnv(process.env.CONTEXTFUL_STORES_JSON);
  const stores = registry.entries.map(({ id, label }) => ({ id, label }));
  const modulePath = process.env.CONTEXTFUL_CONSOLE_ADAPTER_MODULE;
  const live = modulePath ? null : createLiveAnswer({ stores: registry.entries, env: process.env });
  let adapters: HostedAdapters = {
    ...unavailableAdapters(),
    turn: live?.turn ?? unavailableAdapters().turn,
    brief: live?.brief,
    briefBudgetMs: live?.briefBudgetMs,
    redactView: live?.redactView,
  };
  if (modulePath) {
    const absolute = isAbsolute(modulePath) ? modulePath : resolve(modulePath);
    const module: { createAdapters?: AdapterFactory } = await import(pathToFileURL(absolute).href);
    if (!module.createAdapters) throw new Error("console adapter module exports no createAdapters");
    adapters = await module.createAdapters({ stores, env: process.env });
  }
  let origin = process.env.CONTEXTFUL_CONSOLE_ORIGIN ?? `http://${host}:${port}`;
  const app = serveConsole({
    identity: await identity(),
    stores,
    adminCapability: process.env.CONTEXTFUL_ADMIN_CAPABILITY,
    turn: adapters.turn,
    brief: adapters.brief,
    briefBudgetMs: adapters.briefBudgetMs,
    redactView: adapters.redactView,
    read: adapters.read ?? { list: async () => stores },
    control: adapters.control,
  }, () => origin);
  await new Promise<void>((resolveListen, rejectListen) => {
    app.once("error", rejectListen);
    app.listen(port, host, resolveListen);
  });
  const bound = app.address();
  if (!bound || typeof bound === "string") throw new Error("console listener has no address");
  if (!process.env.CONTEXTFUL_CONSOLE_ORIGIN) origin = `http://${host}:${bound.port}`;
  process.stderr.write(`listening on http://${host}:${bound.port}\n`);
}

main().catch((error: unknown) => {
  process.stderr.write(`${error instanceof Error ? error.message : "console startup failed"}\n`);
  process.exitCode = 1;
});
