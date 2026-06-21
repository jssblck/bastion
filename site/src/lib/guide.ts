import { getCollection, type CollectionEntry } from "astro:content";

export type GuideEntry = CollectionEntry<"guide">;

// The guide index (README) is authored with order 0; chapters are 1..n. Keying
// off `order` rather than the file id keeps this robust to how the glob loader
// slugifies "README".
export function isIndex(entry: GuideEntry): boolean {
  return entry.data.order === 0;
}

/** The site route a guide entry renders at (`/guide` for the index). */
export function routeFor(entry: GuideEntry): string {
  return isIndex(entry) ? "/guide" : `/guide/${entry.id}`;
}

/** The path slug used for the raw-markdown endpoint (`index` for the index). */
export function rawSlugFor(entry: GuideEntry): string {
  return isIndex(entry) ? "index" : entry.id;
}

/** Every guide entry, ordered by frontmatter `order`. */
export async function loadGuide(): Promise<GuideEntry[]> {
  const entries = await getCollection("guide");
  return entries.sort((a, b) => a.data.order - b.data.order);
}

export interface Neighbors {
  prev: GuideEntry | null;
  next: GuideEntry | null;
}

/** Previous/next entries in reading order, across the whole ordered chain. */
export function neighbors(entries: GuideEntry[], current: GuideEntry): Neighbors {
  const i = entries.findIndex((e) => e.id === current.id);
  return {
    prev: i > 0 ? entries[i - 1] : null,
    next: i >= 0 && i < entries.length - 1 ? entries[i + 1] : null,
  };
}
