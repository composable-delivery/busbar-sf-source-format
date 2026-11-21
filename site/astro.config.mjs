// @ts-check
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import {
  loadStarlightPlugins,
  packageExportAliases,
  remarkDocLinks,
  remarkStripLeadingH1,
  remarkUxBlocks,
  resolveSiteEnv,
} from '@composable-delivery/generic-ux-kit/node';

const siteDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(siteDir, '..');
const docsRoot = path.join(repoRoot, 'docs');
const generatedRoot = path.join(siteDir, '.generated');

const CANONICAL_URL = 'https://composable-delivery.github.io/sf-source-format/';
const REPO_URL = 'https://github.com/composable-delivery/sf-source-format';

const { site, base, outDir } = resolveSiteEnv({
  defaultSite: 'https://composable-delivery.github.io',
  defaultBase: '/sf-source-format',
});

// Stock Starlight + generic-ux-kit by default (works for forks with no credentials). The MuseLab
// brand layer is opt-in (MUSELAB_BRAND=true, set by the Vercel deployment) and fails loudly if
// it cannot be loaded.
const extra = await loadStarlightPlugins({
  optIn: {
    envVar: 'MUSELAB_BRAND',
    entry: '@composable-delivery/muselab-ux-kit/starlight#muselabStarlightPlugin',
  },
});

/** Serves trailing-slash `/api/.../` `index.html` files during `astro dev`. */
function rustdocDevIndexIntegration(basePath) {
  const cleanBase = basePath.replace(/\/$/, '');
  const apiPrefix = `${cleanBase}/api/`;
  const publicDir = path.join(siteDir, 'public');
  return {
    name: 'rustdoc-dev-index',
    hooks: {
      /** @param {{ server: import('vite').ViteDevServer }} ctx */
      'astro:server:setup': ({ server }) => {
        server.middlewares.stack.unshift({
          route: '',
          handle: (req, res, next) => {
            if (req.url) {
              const [pathname] = req.url.split('?');
              let rel = null;
              if (pathname.startsWith(apiPrefix)) {
                rel = pathname.slice(cleanBase.length);
              } else if (pathname.startsWith('/api/')) {
                rel = pathname;
              }
              if (rel && rel.endsWith('/')) {
                const candidate = path.resolve(publicDir, `.${rel}`, 'index.html');
                const relToPublic = path.relative(publicDir, candidate);
                if (
                  !relToPublic.startsWith('..') &&
                  !path.isAbsolute(relToPublic) &&
                  fs.existsSync(candidate)
                ) {
                  res.statusCode = 200;
                  res.setHeader('Content-Type', 'text/html; charset=utf-8');
                  res.end(fs.readFileSync(candidate));
                  return;
                }
              }
            }
            next();
          },
        });
      },
    },
  };
}

export default defineConfig({
  site,
  base,
  outDir,
  vite: {
    resolve: {
      alias: [
        // docs/*.mdx lives outside the Astro root, so resolve kit imports from the kit's real location.
        ...packageExportAliases(siteDir, '@composable-delivery/generic-ux-kit'),
        // Lets docs/*.mdx import site components without fragile relative paths.
        { find: /^@site\/(.*)$/, replacement: `${siteDir}/$1` },
        {
          find: /^@astrojs\/starlight\/components$/,
          replacement: path.resolve(
            siteDir,
            'node_modules/@astrojs/starlight/dist/components.ts'
          ),
        },
      ],
      modules: [path.resolve(siteDir, 'node_modules'), 'node_modules'],
    },
    ssr: { noExternal: ['@composable-delivery/generic-ux-kit', ...extra.packages] },
  },
  markdown: {
    remarkPlugins: [
      remarkStripLeadingH1,
      // Turns the marked sections of docs/index.md (plain Markdown) into cards.
      remarkUxBlocks,
      [
        remarkDocLinks,
        {
          repoRoot,
          docsRoot,
          generatedRoot,
          base,
          canonicalUrl: CANONICAL_URL,
          repoBlobUrl: `${REPO_URL}/blob/main/`,
          generatedOrigins: {
            'benchmarks.md': 'BENCHMARKS.md',
            'porting.md': 'PORTING.md',
          },
          rootDocs: { 'BENCHMARKS.md': 'benchmarks', 'PORTING.md': 'porting' },
          // /api/... (generated Rustdoc) is not a Markdown page, so link it site-absolute.
          prefixAbsolutePaths: true,
        },
      ],
    ],
  },
  integrations: [
    rustdocDevIndexIntegration(base),
    starlight({
      title: 'sf-source-format',
      description:
        'High-performance Rust engine for Salesforce MDAPI and SFDX source format conversion.',
      social: [{ icon: 'github', label: 'GitHub', href: REPO_URL }],
      plugins: extra.plugins,
      customCss: [
        '@composable-delivery/generic-ux-kit/styles/tokens.css',
        '@composable-delivery/generic-ux-kit/styles/starlight.css',
        './src/styles.css',
      ],
      sidebar: [
        {
          label: 'Getting Started',
          items: [
            { label: 'Overview', link: '/' },
            { label: 'Getting Started', slug: 'getting-started' },
          ],
        },
        {
          label: 'Usage & API',
          items: [
            { label: 'CLI Reference', slug: 'cli-reference' },
            { label: 'Library API Guide', slug: 'library-api' },
          ],
        },
        {
          label: 'Design & Parity',
          items: [
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Porting Notes', slug: 'porting' },
            { label: 'Benchmarks', slug: 'benchmarks' },
          ],
        },
        {
          label: 'Rustdoc API',
          items: [
            { label: 'busbar-sf-source-format', link: '/api/busbar_sf_source_format/' },
            { label: 'registry', link: '/api/busbar_sf_source_format/registry/' },
            { label: 'tree', link: '/api/busbar_sf_source_format/tree/' },
            { label: 'xml', link: '/api/busbar_sf_source_format/xml/' },
            { label: 'resolve', link: '/api/busbar_sf_source_format/resolve/' },
            { label: 'convert', link: '/api/busbar_sf_source_format/convert/' },
          ],
        },
      ],
    }),
  ],
});
