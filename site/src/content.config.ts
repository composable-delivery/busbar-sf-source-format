import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

// Plain Markdown documentation lives in `../docs/`, while root-level `PORTING.md` and
// `BENCHMARKS.md` are synced with Starlight frontmatter into `site/.generated/` at build time.
const SOURCES = [
  ['site/.generated/', ''],
  ['docs/', ''],
];

export const collections = {
  docs: defineCollection({
    loader: glob({
      base: '..',
      pattern: ['docs/**/*.{md,mdx}', 'site/.generated/*.md'],
      generateId: ({ entry }) => {
        const [prefix, idPrefix] = SOURCES.find(([p]) => entry.startsWith(p)) ?? ['', ''];
        return idPrefix + entry.slice(prefix.length).replace(/\.(md|mdx)$/, '');
      },
    }),
    schema: docsSchema(),
  }),
};

