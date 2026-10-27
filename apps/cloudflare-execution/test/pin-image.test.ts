import assert from "node:assert/strict";
import { test } from "node:test";
import { CONTAINER_INSTANCE, pinImage } from "../src/pin-image.ts";

type Started = Record<string, unknown>;

function runtime(images: Record<string, string>) {
  const starts: Started[] = [];
  const container = {
    images,
    running: false,
    start(options?: Started) {
      starts.push(options ?? {});
    },
  };
  return { container, starts };
}

// spec: assurance.deploy-apps.scheduling-policy@c477f7d5
test("every start of a Durable Object-managed container carries the declared image and the standard-1 instance", () => {
  const { container, starts } = runtime({ execution: "registry.example/contextful-execution:abc" });
  pinImage(container as never, CONTAINER_INSTANCE);
  container.start();
  container.start({ env: { A: "1" } });
  assert.equal(CONTAINER_INSTANCE, "standard-1");
  assert.deepEqual(starts, [
    { enableInternet: true, image: "registry.example/contextful-execution:abc", instance: "standard-1" },
    { enableInternet: true, env: { A: "1" }, image: "registry.example/contextful-execution:abc", instance: "standard-1" },
  ]);
});

test("a runtime without images or without a container starts unchanged", () => {
  const { container, starts } = runtime({});
  pinImage(container as never, CONTAINER_INSTANCE);
  container.start({ env: { A: "1" } });
  assert.deepEqual(starts, [{ env: { A: "1" } }]);
  assert.doesNotThrow(() => pinImage(undefined, CONTAINER_INSTANCE));
  const throwing = { get images(): Record<string, string> { throw new Error("no images"); }, start() {} };
  assert.doesNotThrow(() => pinImage(throwing as never, CONTAINER_INSTANCE));
});
