// @ts-check
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import remarkDocLinks from './plugins/remark-doc-links.mjs';
import remarkStripLeadingH1 from './plugins/remark-strip-h1.mjs';
import { loadStarlightPlugins } from './starlight-plugins.mjs';

const siteDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(siteDir, '..');
const docsRoot = path.join(repoRoot, 'docs');
const generatedRoot = path.join(siteDir, '.generated');

const CANONICAL_URL = 'https://composable-delivery.github.io/sf-source-format/';
const REPO_URL = 'https://github.com/composable-delivery/sf-source-format';

const site = process.env.ASTRO_SITE || 'https://composable-delivery.github.io';
const base = process.env.ASTRO_BASE ?? (process.env.VERCEL ? '/' : '/sf-source-format');
const outDir = process.env.ASTRO_OUT_DIR || './dist';

const extra = await loadStarlightPlugins();

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
    ssr: { noExternal: extra.packages },
  },
  markdown: {
    remarkPlugins: [
      remarkStripLeadingH1,
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
      customCss: ['./src/styles.css'],
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
