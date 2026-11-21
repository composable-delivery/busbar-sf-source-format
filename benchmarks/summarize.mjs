#!/usr/bin/env node
// Summarizes the tracked benchmark results (`benchmarks/results/local/*.json`, saved from your own
// machines with save-local-results.mjs, + `benchmarks/results/ci/*.json`, fetched from Actions) into ranges of difference vs Node SDR for the docs Overview.
//
// The Overview content is Markdown in docs/index.md. This script generates the metrics block
// between the `bench:metrics:begin/end` markers in that file, so the numbers are plain Markdown
// (readable on GitHub) yet never typed by hand.
//
//   node benchmarks/summarize.mjs                 print the summary JSON
//   node benchmarks/summarize.mjs --write-docs    regenerate the block in docs/index.md
//   node benchmarks/summarize.mjs --check-docs    exit 1 if the block is stale (used by the docs build)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const localDir = path.join(repoRoot, 'benchmarks', 'results', 'local');
const ciDir = path.join(repoRoot, 'benchmarks', 'results', 'ci');
const overviewFile = path.join(repoRoot, 'docs', 'index.md');
const BLOCK = /<!-- bench:metrics:begin[^>]*-->[\s\S]*?<!-- bench:metrics:end -->/;

const readJson = (f) => JSON.parse(fs.readFileSync(f, 'utf8'));
const range = (values) => ({ min: Math.min(...values), max: Math.max(...values) });

function osName(env = {}) {
  const raw = String(env.os ?? '');
  if (/macOS|Darwin/i.test(raw)) return 'macOS';
  if (/Windows/i.test(raw)) return 'Windows';
  if (/Linux|Ubuntu/i.test(raw)) return 'Linux';
  return raw.split('|')[0].trim() || 'unknown OS';
}

function loadSources() {
  const sources = [];
  if (fs.existsSync(localDir)) {
    for (const f of fs.readdirSync(localDir).filter((n) => n.endsWith('.json')).sort()) {
      const data = readJson(path.join(localDir, f));
      sources.push({ id: `local-${path.basename(f, '.json')}`, kind: 'local', label: `${data.environment?.cpu ?? 'Local machine'} · ${osName(data.environment)}`, date: data.source?.date, data });
    }
  }
  if (fs.existsSync(ciDir)) {
    for (const f of fs.readdirSync(ciDir).filter((n) => n.endsWith('.json')).sort()) {
      const data = readJson(path.join(ciDir, f));
      const runner = data.source?.runner ?? path.basename(f, '.json');
      sources.push({ id: `ci-${runner}`, kind: 'ci', label: `GitHub Actions · ${runner}`, date: data.source?.date, url: data.source?.url, data });
    }
  }
  return sources;
}

export function summarize() {
  const sources = loadSources();
  if (sources.length === 0) throw new Error('No benchmark results found in benchmarks/results/{local,ci}/.');

  const inProc = [];
  const cold = [];
  const mem = [];
  const suiteMem = [];
  const coldStart = [];
  for (const s of sources) {
    for (const w of s.data.part_a?.workloads ?? []) {
      if (w.speedup_median > 0) inProc.push({ src: s.id, name: w.name, v: w.speedup_median });
    }
    const pm = s.data.part_a?.process_memory;
    if (pm?.rust_peak_footprint_bytes && pm?.node_peak_footprint_bytes) {
      suiteMem.push(pm.node_peak_footprint_bytes / pm.rust_peak_footprint_bytes);
    }
    for (const w of Array.isArray(s.data.part_b) ? s.data.part_b : []) {
      if (!w.rust_cli || !w.sf_cli) continue;
      cold.push({ src: s.id, name: w.name, v: w.sf_cli.median_ms / w.rust_cli.median_ms });
      if (w.rust_cli.max_rss_bytes && w.sf_cli.max_rss_bytes) {
        mem.push({ src: s.id, name: w.name, v: w.sf_cli.max_rss_bytes / w.rust_cli.max_rss_bytes });
      }
      coldStart.push(w.rust_cli.median_ms);
    }
  }
  const scenarios = (list) => new Set(list.map((x) => x.name.replace(/\s*\(.*\)$/, '') + '|' + (x.name.match(/\((.*)\)/)?.[1] ?? ''))).size;
  const platforms = (list) => new Set(list.map((x) => x.src)).size;
  const vals = (list) => list.map((x) => x.v);

  const summary = {
    sdrVersion: sources.find((s) => s.data.environment?.sdr_version)?.data.environment.sdr_version ?? null,
    sources: sources.map(({ id, kind, label, date, url }) => ({ id, kind, label, date, url })),
    inProcess: { speedup: range(vals(inProc)), scenarios: scenarios(inProc), platforms: platforms(inProc) },
    coldCli: {
      speedup: cold.length ? range(vals(cold)) : null,
      memory: mem.length ? range(vals(mem)) : null,
      scenarios: scenarios(cold),
      platforms: platforms(cold),
    },
    suiteMemory: suiteMem.length ? range(suiteMem) : null,
    coldStartMs: coldStart.length ? Math.min(...coldStart) : null,
  };
  return summary;
}

const fmt = (v) => (v >= 10 ? String(Math.round(v)) : v.toFixed(1));
const span = (r) => (fmt(r.min) === fmt(r.max) ? `${fmt(r.min)}×` : `${fmt(r.min)}–${fmt(r.max)}×`);
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

/** The generated Markdown: a `ux:metrics` block plus one sentence on what was measured. */
export function renderMetricsMarkdown(summary) {
  const { inProcess, coldCli, suiteMemory, coldStartMs } = summary;
  const memory = coldCli.memory ?? suiteMemory;
  const card = (title, value, unit, detail) => `### ${title}\n**${value}** ${unit}\n${detail}`;
  const cards = [
    card('Conversion speed', span(inProcess.speedup), 'faster',
      `In-process, across ${plural(inProcess.scenarios, 'scenario')} on ${plural(inProcess.platforms, 'platform')}.`),
    coldCli.speedup &&
      card('Cold start in CI', span(coldCli.speedup), 'faster',
        `Whole command vs the \`sf\` CLI, across ${plural(coldCli.scenarios, 'scenario')} on ${plural(coldCli.platforms, 'platform')}.`),
    memory &&
      card('Memory use', span(memory), 'less memory',
        coldCli.memory
          ? `Peak memory per run vs the \`sf\` CLI, across ${plural(coldCli.scenarios, 'scenario')}.`
          : 'Peak memory across the full benchmark suite.'),
    coldStartMs != null &&
      card('No runtime to boot', `${Math.max(1, Math.round(coldStartMs))} ms`, 'to start',
        'A single native binary. No Node.js, no npm install, no warm-up.'),
  ].filter(Boolean);
  const sources = summary.sources.map((x) => x.label).join('; ');
  return [
    '<!-- bench:metrics:begin (generated by benchmarks/summarize.mjs; do not edit by hand) -->',
    '<!-- ux:metrics -->',
    cards.join('\n\n'),
    '<!-- /ux:metrics -->',
    '',
    `Compared with \`@salesforce/source-deploy-retrieve\` ${summary.sdrVersion} and the \`sf\` CLI. Measured on ${sources}. See the [full results and methodology](../BENCHMARKS.md).`,
    '<!-- bench:metrics:end -->',
  ].join('\n');
}

/** Regenerates (write) or verifies (check) the block in docs/index.md. */
export function syncOverview({ write = false } = {}) {
  const current = fs.readFileSync(overviewFile, 'utf8');
  if (!BLOCK.test(current)) {
    throw new Error('docs/index.md is missing the <!-- bench:metrics:begin --> ... <!-- bench:metrics:end --> markers.');
  }
  const next = current.replace(BLOCK, () => renderMetricsMarkdown(summarize()));
  const changed = next !== current;
  if (changed && write) fs.writeFileSync(overviewFile, next);
  return { changed, file: overviewFile };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.includes('--write-docs')) {
    const { changed } = syncOverview({ write: true });
    console.log(changed ? 'docs/index.md metrics block updated.' : 'docs/index.md metrics block already current.');
  } else if (args.includes('--check-docs')) {
    process.exit(syncOverview().changed ? 1 : 0);
  } else {
    console.log(JSON.stringify(summarize(), null, 2));
  }
}
