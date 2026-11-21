import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertNoTables } from '@composable-delivery/generic-ux-kit/node';
import { syncOverview } from '../../benchmarks/summarize.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const siteDir = path.resolve(scriptsDir, '..');
const repoRoot = path.resolve(siteDir, '..');
const generatedDir = path.join(siteDir, '.generated');
const targetDocDir = path.join(repoRoot, 'target', 'doc');
const publicApiDir = path.join(siteDir, 'public', 'api');

const ROOT_DOCS = [
  {
    source: 'BENCHMARKS.md',
    target: 'benchmarks.md',
    title: 'Benchmarks',
    description:
      'Performance comparison of sf-source-format against @salesforce/source-deploy-retrieve and the sf CLI.',
  },
  {
    source: 'PORTING.md',
    target: 'porting.md',
    title: 'Porting Notes',
    description:
      'TypeScript-to-Rust architecture mapping, design decisions, and upstream SDR 13.13.0 parity notes.',
  },
];

fs.mkdirSync(generatedDir, { recursive: true });

// The Overview is an introduction for new users: benchmark tables belong on the Benchmarks page.
// Fail the build rather than let one creep back in.
assertNoTables(path.join(repoRoot, 'docs', 'index.md'), 'Show ranges as metric cards; put tables on the Benchmarks page.');

// The Overview's metric cards are Markdown generated from the benchmark results. Refuse to build
// with stale numbers (they must agree with the Benchmarks page).
if (syncOverview().changed) {
  throw new Error(
    'docs/index.md benchmark cards are out of date with benchmarks/results*. Run: node benchmarks/summarize.mjs --write-docs'
  );
}

for (const doc of ROOT_DOCS) {
  const raw = fs.readFileSync(path.join(repoRoot, doc.source), 'utf-8');
  const body = raw.replace(/^# [^\n]+\n+/, '');
  const frontmatter = [
    '---',
    `title: ${JSON.stringify(doc.title)}`,
    `description: ${JSON.stringify(doc.description)}`,
    '---',
    '',
  ].join('\n');
  fs.writeFileSync(path.join(generatedDir, doc.target), `${frontmatter}${body}`);
  console.log(`[sync-docs] Synced ${doc.source} -> site/.generated/${doc.target}`);
}

if (process.env.SKIP_RUSTDOC_SYNC !== '1') {
  const sentinel = path.join(targetDocDir, 'busbar_sf_source_format', 'index.html');
  if (!fs.existsSync(sentinel)) {
    console.log('[sync-docs] Rustdoc not found in target/doc; running cargo doc...');
    const res = spawnSync(
      'cargo',
      ['doc', '--workspace', '--no-deps', '--all-features'],
      { cwd: repoRoot, stdio: 'inherit' }
    );
    if (res.status !== 0) {
      console.warn('[sync-docs] cargo doc failed or unavailable; skipping Rustdoc API sync.');
    }
  }

  if (fs.existsSync(sentinel)) {
    fs.rmSync(publicApiDir, { recursive: true, force: true });
    fs.mkdirSync(publicApiDir, { recursive: true });
    fs.cpSync(targetDocDir, publicApiDir, { recursive: true });
    fs.writeFileSync(
      path.join(publicApiDir, 'index.html'),
      [
        '<!doctype html>',
        '<meta http-equiv="refresh" content="0; url=busbar_sf_source_format/index.html">',
        '<link rel="canonical" href="busbar_sf_source_format/index.html">',
        '',
      ].join('\n')
    );
    console.log('[sync-docs] Synced target/doc -> site/public/api');
  }
}
