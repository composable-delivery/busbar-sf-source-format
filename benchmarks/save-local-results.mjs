#!/usr/bin/env node
// Saves a local benchmark run (the gitignored scratch file benchmarks/results.json written by
// run-comparison.py) into the tracked benchmarks/results/local/<machine>.json, so the docs and CI
// see the same numbers as your machine. Then refresh the Overview with:
//   node benchmarks/summarize.mjs --write-docs
//
// Usage: node benchmarks/save-local-results.mjs [path/to/results.json]
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const input = process.argv[2] ?? path.join(repoRoot, 'benchmarks', 'results.json');
const data = JSON.parse(fs.readFileSync(input, 'utf8'));

const env = data.environment ?? {};
const os = /macOS|Darwin/i.test(env.os ?? '') ? 'macos' : /Windows/i.test(env.os ?? '') ? 'windows' : 'linux';
const slug = `${env.cpu ?? 'machine'}-${os}`.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

data.source = { kind: 'local', machine: slug, date: new Date().toISOString() };
const outDir = path.join(repoRoot, 'benchmarks', 'results', 'local');
fs.mkdirSync(outDir, { recursive: true });
const out = path.join(outDir, `${slug}.json`);
fs.writeFileSync(out, `${JSON.stringify(data, null, 2)}\n`);
console.log(`[bench] saved ${path.relative(repoRoot, out)}`);
