import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { registryFromEnv, routeRequest } from "../src/index.ts";

const KEY = "audience-key-mirror";
const digest = (text: string) => createHash("sha256").update(text).digest("hex");
const BASIN = "https://sql.example/basin-sql/query/exports";

function entry(basin: Record<string, unknown>) {
  return registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example", basin: { endpoint: BASIN, ...basin } }]));
}

const secrets: Record<string, string> = {
  FIELD_NOTES_BASIN_SQL_TOKEN: "basin-read-token",
  FIELD_NOTES_BASIN_AUDIENCES: JSON.stringify({ [digest(KEY)]: "mirror" }),
};

/** A route over `registry` recording what reached the Container and what reached Basin SQL. */
function harness(registry: ReturnType<typeof registryFromEnv>) {
  const seen = { container: [] as string[], basin: [] as { url: string; auth: string | null; query: string }[] };
  const route = (request: Request) =>
    routeRequest(request, registry, async (_entry, proxied) => {
      seen.container.push(new URL(proxied.url).pathname);
      return new Response("container", { status: 200 });
    }, {
      secret: (name) => secrets[name],
      basin: async (url, init) => {
        const body = JSON.parse(String(init.body)) as { query: string };
        seen.basin.push({ url, auth: new Headers(init.headers).get("authorization"), query: body.query });
        return Response.json({ success: true, result: { rows: [] } });
      },
    });
  return { seen, route };
}

const query = (key: string | null, body: unknown, path = "/stores/field-notes/query") =>
  new Request(`https://gateway.example${path}`, { method: "POST", headers: key ? { authorization: `Bearer ${key}`, "content-type": "application/json" } : {}, body: JSON.stringify(body) });

// spec: run.export-iceberg.route@853e145e
test("a caller whose audience key maps to a namespace reads Basin SQL, and every other caller the Container", async () => {
  const { seen, route } = harness(entry({ templates: { count: "SELECT count(*) AS n FROM {namespace}.{table}" } }));
  const mapped = await route(query(KEY, { template: "count", table: "spans" }));
  assert.equal(mapped.status, 200);
  assert.deepEqual(seen.basin, [{ url: BASIN, auth: "Bearer basin-read-token", query: "SELECT count(*) AS n FROM mirror.spans" }]);
  for (const key of ["some-contextful-credential", null]) {
    const other = await route(query(key, { template: "count", table: "spans" }));
    assert.equal(await other.text(), "container");
  }
  assert.deepEqual(seen.container, ["/query", "/query"]);
  assert.equal(seen.basin.length, 1, "an unmapped caller never reaches Basin SQL");
});

// spec: run.export-iceberg.audience@c7eb8d69
// spec: run.export-iceberg.audience-key@1335e4dc
// spec: run.export-iceberg.query-refused@dc7ef31a
test("the gateway derives the namespace from the audience key, and the caller never names one", async () => {
  const { seen, route } = harness(entry({ templates: { count: "SELECT count(*) AS n FROM {namespace}.{table}" } }));
  for (const table of ["other.spans", "spans; DROP TABLE x", "Spans"]) {
    const refused = await route(query(KEY, { template: "count", table }));
    assert.equal(refused.status, 400, table);
    assert.match(await refused.text(), /ExportQueryRefused/, table);
  }
  const named = await route(query(KEY, { template: "count", table: "spans", namespace: "other" }));
  assert.equal(named.status, 400);
  assert.equal(seen.basin.length, 0);
  assert.equal(seen.container.length, 0, "a mapped caller is never sent to the Container");
});

// spec: run.export-iceberg.caller-sql@be1c44db
test("caller-written SQL reaches Basin SQL only on a single-audience bucket; elsewhere the templates run", async () => {
  const shared = harness(entry({ templates: { count: "SELECT count(*) AS n FROM {namespace}.{table}" } }));
  const refused = await shared.route(query(KEY, { sql: "SELECT * FROM other.spans" }));
  assert.equal(refused.status, 403);
  assert.match(await refused.text(), /ExportQueryRefused/);
  const unknown = await shared.route(query(KEY, { template: "drop-all", table: "spans" }));
  assert.equal(unknown.status, 400);
  assert.equal(shared.seen.basin.length, 0);

  const single = harness(entry({ caller_sql: true }));
  const passed = await single.route(query(KEY, { sql: "SELECT label, count(*) FROM mirror.spans GROUP BY label" }));
  assert.equal(passed.status, 200);
  assert.deepEqual(single.seen.basin.map((b) => b.query), ["SELECT label, count(*) FROM mirror.spans GROUP BY label"]);
});

test("a mapped caller reaches Basin SQL through POST /query alone", async () => {
  const { seen, route } = harness(entry({ caller_sql: true }));
  const mcp = await route(query(KEY, { sql: "SELECT 1" }, "/stores/field-notes/mcp"));
  assert.equal(mcp.status, 404);
  assert.equal(seen.basin.length + seen.container.length, 0);
});

// spec: run.export-iceberg.gateway-entry@fc696c11
test("a basin entry names its endpoint as an https URL and authors no secret name", () => {
  assert.equal(entry({ caller_sql: true }).entries[0].basin?.tokenName, "FIELD_NOTES_BASIN_SQL_TOKEN");
  assert.equal(entry({ caller_sql: true }).entries[0].basin?.audiencesName, "FIELD_NOTES_BASIN_AUDIENCES");
  for (const basin of [{ endpoint: "not a url" }, { endpoint: "ftp://sql.example" }, { endpoint: BASIN, templates: { x: 1 } }, { endpoint: BASIN, caller_sql: "yes" }]) {
    const registry = registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example", basin }]));
    assert.deepEqual(registry.problems, [{ identifier: "StoreEntryMalformed", entry: 0 }], JSON.stringify(basin));
  }
  assert.throws(() => registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example", basin: { endpoint: BASIN, tokenName: "X" } }])), /StoreNameAuthored/);
});
