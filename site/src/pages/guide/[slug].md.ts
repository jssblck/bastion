import type { APIRoute, GetStaticPaths } from "astro";
import { loadGuide, rawSlugFor } from "../../lib/guide";

// Serve every guide page's raw Markdown at a predictable `.md` URL (e.g.
// /guide/concepts.md). This is the representation agents and crawlers probe for,
// and the exact text the "Copy page" button copies.
export const getStaticPaths = (async () => {
  const entries = await loadGuide();
  return entries.map((entry) => ({
    params: { slug: rawSlugFor(entry) },
    props: { body: entry.body ?? "" },
  }));
}) satisfies GetStaticPaths;

export const GET: APIRoute = ({ props }) =>
  new Response(`${(props.body as string).trim()}\n`, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
