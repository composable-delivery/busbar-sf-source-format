#!/usr/bin/env node
// Summarizes benchmark results (local `benchmarks/results.json` + CI results in
// `benchmarks/results/ci/*.json`) into ranges of difference vs Node SDR, for the docs Overview.
// Writes site/.generated/benchmark-summary.json.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const localFile = path.join(repoRoot, 'benchmarks', 'results.json');
const ciDir = path.join(repoRoot, 'benchmarks', 'results', 'ci');
const outFile = path.join(repoRoot, 'site', '.generated', 'benchmark-summary.json');

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
  if (fs.existsSync(localFile)) {
    const data = readJson(localFile);
    sources.push({ id: 'local', kind: 'local', label: `${data.environment?.cpu ?? 'Local machine'} · ${osName(data.environment)}`, data });
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
  if (sources.length === 0) throw new Error('No benchmark results found in benchmarks/.');

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

export function writeSummary() {
  const summary = summarize();
  fs.mkdirSync(path.dirname(outFile), { recursive: true });
  fs.writeFileSync(outFile, `${JSON.stringify(summary, null, 2)}\n`);
  return summary;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify(writeSummary(), null, 2));
}
