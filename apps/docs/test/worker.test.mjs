import assert from "node:assert/strict";
import test from "node:test";
import worker from "../src/worker.ts";

test("the former documentation homepage redirects to the specification", async () => {
  const response = await worker.fetch(new Request("https://spec.contextful.work/?from=bookmark"), {});
  assert.equal(response.status, 301);
  assert.equal(response.headers.get("location"), "https://contextful.fractalbox.dev/docs/specs/?from=bookmark");
});

test("former documentation URLs preserve their paths and query strings", async () => {
  const response = await worker.fetch(new Request("https://spec.contextful.work/cards/store/?from=bookmark"), {});
  assert.equal(response.status, 301);
  assert.equal(response.headers.get("location"), "https://contextful.fractalbox.dev/cards/store/?from=bookmark");
});

test("canonical requests reach static assets without a hostname redirect", async () => {
  const request = new Request("https://contextful.fractalbox.dev/docs/");
  const expected = new Response("Documentation", { status: 200 });
  const response = await worker.fetch(request, { ASSETS: { fetch: async (received) => {
    assert.equal(received, request);
    return expected;
  } } });
  assert.equal(response, expected);
});
