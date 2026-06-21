# Bastion marketing site

The source for [bastion.jessica.black](https://bastion.jessica.black): a single-page
explainer for what Bastion is and why you'd want it.

It is a static [Astro](https://astro.build) site. The Rust crate in the repository
root is untouched by it; the Node toolchain is scoped entirely to this directory.

## Develop

```sh
cd site
npm install
npm run dev      # http://localhost:4321
```

```sh
npm run build    # static output to site/dist
npm run preview  # serve the production build locally
npm run check    # astro type/diagnostics check
```

## Structure

- `src/pages/index.astro` composes the page from the section components.
- `src/components/` holds one component per section (`Hero`, `Problem`,
  `Reviewers`, `Gate`, `Govern`, `Mirror`, `Trust`, `Install`) plus shared atoms
  (`Nav`, `Footer`, `SectionHeading`, `Artifact`, `Logo`).
- `src/styles/tokens.css` is the design system: colors (OKLCH), type scale,
  spacing, motion. `src/styles/global.css` is the reset, base type, and shared
  utilities.
- `public/` holds static assets: `CNAME` (custom domain), `favicon.svg`, and
  `og.png` (the Open Graph card).

Design intent and the visual system are documented in `PRODUCT.md` and `DESIGN.md`.

## The Open Graph image

`public/og.png` is generated from an on-brand template. To regenerate it (for
example after changing the headline), install Playwright once and run the script:

```sh
npm install --no-save playwright && npx playwright install chromium
node scripts/og-gen.mjs
```

Playwright is intentionally not a tracked dependency: the committed `og.png` is the
source of truth, and the generator is only needed when it changes.

## Deploying

Pushes to `main` that touch `site/**` trigger
[`.github/workflows/site.yml`](../.github/workflows/site.yml), which builds the site
and deploys it to GitHub Pages.

A custom domain needs three things set up once, outside this repo:

1. **DNS:** a `CNAME` record for `bastion.jessica.black` pointing at
   `jssblck.github.io` (or four `A` records to the GitHub Pages apex IPs if you
   ever serve a bare apex instead of a subdomain).
2. **Repo settings -> Pages:** set the build source to **GitHub Actions**, and set
   the custom domain to `bastion.jessica.black` (this matches `public/CNAME`, which
   the build copies into `dist/`). Enable **Enforce HTTPS** once the certificate is
   issued.
3. The first successful run of the workflow publishes the site.
