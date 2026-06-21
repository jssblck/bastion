// @ts-check
import { defineConfig } from "astro/config";

// Custom domain (CNAME in public/) serves the site at the root path, so no
// `base` is needed. `site` powers canonical URLs, sitemap, and Open Graph tags.
export default defineConfig({
  site: "https://bastion.jessica.black",
  trailingSlash: "never",
  build: {
    inlineStylesheets: "auto",
  },
});
