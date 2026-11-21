// Fails the docs build if any hard-coded docs URL disagrees with site/docs.config.mjs.
// Markdown, install scripts and vercel.json cannot import the config, so they are checked here.
import fs from 'node:fs';
import path from 'node:path';
import { DOCS_ORIGIN, DOCS_BASE, REPO_NAME } from '../docs.config.mjs';

export function checkDocsUrl(repoRoot) {
  const files = [
    path.join(repoRoot, 'README.md'),
    path.join(repoRoot, 'site', 'public', 'install.sh'),
    path.join(repoRoot, 'site', 'public', 'install.ps1'),
    ...fs.readdirSync(path.join(repoRoot, 'docs')).filter((f) => /\.mdx?$/.test(f)).map((f) => path.join(repoRoot, 'docs', f)),
  ];
  const problems = [];
  const host = DOCS_ORIGIN.replace(/^https?:\/\//, '').replace(/\./g, '\\.');
  const re = new RegExp(`https?://${host}/([^/\\s)"'\`]*)`, 'g');
  for (const file of files) {
    for (const [, first] of fs.readFileSync(file, 'utf8').matchAll(re)) {
      if (first !== REPO_NAME) problems.push(`${path.relative(repoRoot, file)}: ${DOCS_ORIGIN}/${first}… (expected ${DOCS_ORIGIN}${DOCS_BASE}/)`);
    }
  }
  const vercel = fs.readFileSync(path.join(repoRoot, 'site', 'vercel.json'), 'utf8');
  if (!vercel.includes(`"${DOCS_BASE}/`)) problems.push(`site/vercel.json: no rewrite/redirect under "${DOCS_BASE}/"`);
  for (const m of vercel.matchAll(/"(\/[^"/]+)\/(?::path\*\/?)?"/g)) {
    if (m[1] !== DOCS_BASE) problems.push(`site/vercel.json: path ${m[1]} does not match ${DOCS_BASE}`);
  }
  if (problems.length) {
    throw new Error(`Docs URL does not match site/docs.config.mjs:\n  - ${problems.join('\n  - ')}`);
  }
}
