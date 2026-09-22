import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { z } from "astro/zod";

// Contract files carry `contract` and `owns`, guides carry `contract`; roadmap, status,
// records and cards carry none.
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
  guide: defineCollection({
    loader: glob({ pattern: "*.md", base: "../../spec/guide", generateId: stem }),
    schema,
  }),
  cards: defineCollection({
    loader: glob({ pattern: "*.md", base: "../../spec/cards", generateId: stem }),
    schema,
  }),
};
