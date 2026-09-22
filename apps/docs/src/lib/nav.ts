import { getCollection, type CollectionEntry } from "astro:content";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parse } from "smol-toml";
import { SPEC_DIR } from "./corpus.mjs";

type Entry = CollectionEntry<"spec"> | CollectionEntry<"adr"> | CollectionEntry<"guide"> | CollectionEntry<"cards">;

export interface NavItem {
  href: string;
  label: string;
  hint?: string;
}

export interface NavGroup {
  label: string;
  items: NavItem[];
  collapsed?: boolean;
}

const plain = (s: string) =>
  s
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[`*_]/g, "")
    .replace(/\s+/g, " ")
    .trim();

export const titleOf = (entry: Entry) => plain(/^# (.+)$/m.exec(entry.body ?? "")?.[1] ?? entry.id);

/** The first prose paragraph after the H1, cut to 160 characters. */
export const descriptionOf = (entry: Entry) => {
  const afterTitle = (entry.body ?? "").split(/^# .+$/m)[1] ?? "";
  const para = afterTitle
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .find((p) => p && !/^(#|\||```|- |\*\*Status)/.test(p));
  const text = plain(para ?? titleOf(entry));
  return text.length > 160 ? `${text.slice(0, 157).replace(/\s+\S*$/, "")}…` : text;
};

interface ContractMeta {
  contract: string;
  title: string;
  gloss?: string;
}

/** `spec/terms/contract.toml`, keyed by file stem (`10-store`). */
export const contractsByFile = (): Map<string, ContractMeta> => {
  const out = new Map<string, ContractMeta>();
  try {
    const toml = parse(readFileSync(join(SPEC_DIR, "terms/contract.toml"), "utf8")) as {
      contract?: Record<string, { title?: string; gloss?: string; files?: string[] }>;
    };
    for (const [contract, c] of Object.entries(toml.contract ?? {})) {
      for (const f of c.files ?? []) {
        const stem = f.replace(/^spec\//, "").replace(/\.md$/, "");
        out.set(stem, { contract, title: c.title ?? contract, gloss: c.gloss });
      }
    }
  } catch {
    // The index falls back to each file's H1 when the registry is unreadable.
  }
  return out;
};

/** A contract's guide, card and files, for the links a page carries to its companions. */
export const companions = async (contract: string | undefined) => {
  if (!contract) return undefined;
  const [guides, cards, spec] = await Promise.all([getCollection("guide"), getCollection("cards"), getCollection("spec")]);
  const byFile = contractsByFile();
  return {
    contract,
    guide: guides.some((g) => g.id === contract) ? `/guide/${contract}/` : undefined,
    card: cards.some((c) => c.id === contract) ? `/cards/${contract}/` : undefined,
    files: spec
      .filter((e) => (e.data.contract ?? byFile.get(e.id)?.contract) === contract)
      .sort((a, b) => a.id.localeCompare(b.id))
      .map((e) => ({ href: `/spec/${e.id}/`, file: `${e.id}.md` })),
  };
};

const byRecordOrder = (a: string, b: string) => a.localeCompare(b, "en", { numeric: true });

export const navigation = async (): Promise<NavGroup[]> => {
  const spec = (await getCollection("spec")).sort((a, b) => a.id.localeCompare(b.id));
  const adr = (await getCollection("adr")).sort((a, b) => byRecordOrder(a.id, b.id));
  const guides = (await getCollection("guide")).sort((a, b) => a.id.localeCompare(b.id));
  const cards = (await getCollection("cards")).sort((a, b) => a.id.localeCompare(b.id));
  const contracts = contractsByFile();
  const item = (base: string) => (e: Entry): NavItem => ({ href: `/${base}/${e.id}/`, label: titleOf(e) });
  const adrItem = (e: Entry): NavItem => ({
    href: `/adr/${e.id}/`,
    label: titleOf(e).replace(/^(P\d+|D\d+|A-[a-z]+)\s+—\s+/, ""),
    hint: e.id.startsWith("A-") ? e.id : e.id.split("-")[0],
  });

  const isContract = (e: Entry) => Boolean(e.data.contract) || contracts.has(e.id);
  const groups: NavGroup[] = [
    { label: "Guides", items: guides.map((e) => ({ href: `/guide/${e.id}/`, label: titleOf(e) })) },
    {
      label: "Contracts",
      items: spec.filter(isContract).map((e) => ({ ...item("spec")(e), hint: e.id.split("-")[0] })),
    },
    {
      label: "Cards",
      items: cards.map((e) => ({ href: `/cards/${e.id}/`, label: titleOf(e).replace(/\s+—\s+card$/, "") })),
      collapsed: true,
    },
    { label: "Plan", items: spec.filter((e) => !isContract(e)).map(item("spec")) },
    { label: "Principles", items: adr.filter((e) => /^P\d/.test(e.id)).map(adrItem) },
    { label: "Contract ADRs", items: adr.filter((e) => e.id.startsWith("A-")).map(adrItem) },
    { label: "Decision records", items: adr.filter((e) => /^D\d/.test(e.id)).map(adrItem), collapsed: true },
    { label: "Other records", items: adr.filter((e) => !/^(P\d|D\d|A-)/.test(e.id)).map(adrItem) },
  ];
  return groups.filter((g) => g.items.length);
};
