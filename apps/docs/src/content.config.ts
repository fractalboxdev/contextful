import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { z } from "astro/zod";

// Contract files carry `contract` and `owns`; roadmap, status and records carry none.
const schema = z
  .object({
    contract: z.string().optional(),
    owns: z.array(z.string()).optional(),
  })
  .loose();

const stem = ({ entry }: { entry: string }) => entry.replace(/\.md$/, "");

export const collections = {
  spec: defineCollection({
    loader: glob({ pattern: "*.md", base: "../../spec", generateId: stem }),
    schema,
  }),
  adr: defineCollection({
    loader: glob({ pattern: "*.md", base: "../../spec/adr", generateId: stem }),
    schema,
  }),
};
