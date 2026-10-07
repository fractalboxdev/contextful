import type { Operator } from "./index.ts";
import { createBrowse } from "./browse.ts";
import { openReader, type LiveOptions } from "./live.ts";

type BrowseInput = { operator: Operator; store: string; asOf?: string };

export function createLiveBrowse(options: LiveOptions) {
  async function session(input: BrowseInput) {
    const { call } = await openReader(options, input.operator, input.store);
    return createBrowse({ call: async (name, args) => ({ structuredContent: await call(name, args) }) });
  }

  return {
    async discover(input: BrowseInput) {
      return (await session(input)).discover({ asOf: input.asOf });
    },
    async preview(input: BrowseInput & { path: string }) {
      return (await session(input)).preview({ asOf: input.asOf, path: input.path });
    },
  };
}
