#!/usr/bin/env node
// Summarizes the tracked benchmark results (`benchmarks/results/local/*.json`, saved from your own
// machines with save-local-results.mjs, + `benchmarks/results/ci/*.json`, fetched from Actions) into ranges of difference vs Node SDR for the docs Overview.
//
// The Overview content is Markdown in docs/index.md. This script generates the metrics block
// between the `bench:metrics:begin/end` markers in that file, so the numbers are plain Markdown
// (readable on GitHub) yet never typed by hand.
//
//   node benchmarks/summarize.mjs                 print the summary JSON
//   node benchmarks/summarize.mjs --write-docs    regenerate the generated blocks (docs/index.md, BENCHMARKS.md)
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
  const { inProcess, coldCli, suiteMemory } = summary;
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

const workloadsFile = path.join(repoRoot, 'benchmarks', 'workloads.json');
const benchmarksFile = path.join(repoRoot, 'BENCHMARKS.md');
const TABLES = /<!-- bench:tables:begin[^>]*-->[\s\S]*?<!-- bench:tables:end -->/;

const esc = (t) => String(t).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const info = (tip) => `<span class="ux-info" tabindex="0" role="img" aria-label="${esc(tip)}" data-tip="${esc(tip)}">ⓘ</span>`;
const num = (n, d = 0) => n.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d });
const ms = (v) => (v >= 100 ? `${num(v)} ms` : v >= 10 ? `${num(v, 1)} ms` : `${num(v, 2)} ms`);
const mib = (b) => `${num(b / 1048576, 1)} MiB`;
const times = (v) => `${v >= 100 ? num(v) : v >= 10 ? num(v, 1) : num(v, 2)}×`;
// Skew: how far the slow end (p95) strays from the typical (median) run, as a percentage of the median.
const skew = (t) => `+${num(Math.max(0, ((t.p95_ms - t.median_ms) / t.median_ms) * 100))}%`;

const SKEW_TIP = 'Skew = (p95 − median) ÷ median. How far the slowest typical runs stray from the median run. Lower is steadier.';
const SPEEDUP_TIP = 'Median time of the baseline ÷ median time of sf-source-format. Higher is faster.';
const MEMORY_TIP = 'Peak resident memory of the baseline ÷ peak resident memory of sf-source-format.';

function workloadCell(name, descriptions) {
  const m = /^(.*?)\s*\((.*)\)\s*$/.exec(name);
  const base = m ? m[1] : name;
  const dir = (m ? m[2] : '').replace(/->/g, '→');
  const tip = descriptions[base];
  return `<td><code>${esc(base)}</code>${tip ? info(tip) : ''}${dir ? `<small>${esc(dir)}</small>` : ''}</td>`;
}

function inProcessTable(workloads, descriptions) {
  const rows = workloads.map((w) =>
    `<tr>${workloadCell(w.name, descriptions)}<td><strong>${times(w.speedup_median)}</strong></td>` +
    `<td class="ux-group-start">${ms(w.rust.median_ms)}</td><td>${skew(w.rust)}</td>` +
    `<td class="ux-group-start">${ms(w.node.median_ms)}</td><td>${skew(w.node)}</td></tr>`);
  return [
    '<table class="ux-grouped">', '<thead>',
    `<tr><th rowspan="2">Workload</th><th rowspan="2">Speedup${info(SPEEDUP_TIP)}</th><th colspan="2">sf-source-format</th><th colspan="2">Node SDR</th></tr>`,
    `<tr><th>Median</th><th>Skew${info(SKEW_TIP)}</th><th>Median</th><th>Skew${info(SKEW_TIP)}</th></tr>`,
    '</thead>', '<tbody>', ...rows, '</tbody>', '</table>',
  ].join('\n');
}

function coldCliTable(workloads, descriptions) {
  const rows = workloads.map((w) => {
    const speed = w.sf_cli.median_ms / w.rust_cli.median_ms;
    const mem = w.sf_cli.max_rss_bytes / w.rust_cli.max_rss_bytes;
    return `<tr>${workloadCell(w.name, descriptions)}<td><strong>${times(speed)}</strong></td><td>${times(mem)}</td>` +
      `<td class="ux-group-start">${ms(w.rust_cli.median_ms)}</td><td>${skew(w.rust_cli)}</td><td>${mib(w.rust_cli.max_rss_bytes)}</td>` +
      `<td class="ux-group-start">${ms(w.sf_cli.median_ms)}</td><td>${skew(w.sf_cli)}</td><td>${mib(w.sf_cli.max_rss_bytes)}</td></tr>`;
  });
  return [
    '<table class="ux-grouped">', '<thead>',
    `<tr><th rowspan="2">Workload</th><th rowspan="2">Speedup${info(SPEEDUP_TIP)}</th><th rowspan="2">Less memory${info(MEMORY_TIP)}</th><th colspan="3">sf-source-format</th><th colspan="3"><code>sf</code> CLI</th></tr>`,
    `<tr><th>Median</th><th>Skew${info(SKEW_TIP)}</th><th>Peak RSS</th><th>Median</th><th>Skew${info(SKEW_TIP)}</th><th>Peak RSS</th></tr>`,
    '</thead>', '<tbody>', ...rows, '</tbody>', '</table>',
  ].join('\n');
}

function environmentLine(src) {
  const e = src.data.environment ?? {};
  const known = (v) => v && v !== 'unavailable';
  const bits = [
    known(e.cpu) && e.cpu, known(e.cores) && `${e.cores} cores`, known(e.ram_bytes) && `${num(Number(e.ram_bytes) / 1073741824)} GiB RAM`,
    known(e.os) && (e.os.includes('|') ? osName(e) : e.os),
    e.rustc && e.rustc.split(' ').slice(0, 2).join(' '), e.node && `Node ${e.node}`,
    src.date && `measured ${src.date.slice(0, 10)}`,
  ].filter(Boolean);
  return bits.join(' · ');
}

function sourceSection(src, descriptions, level) {
  const h = '#'.repeat(level);
  const parts = [`${h} ${src.label}`, '', environmentLine(src), ''];
  const a = src.data.part_a?.workloads ?? [];
  if (a.length) parts.push(`${h}# In-process conversion`, '', inProcessTable(a, descriptions), '');
  const b = Array.isArray(src.data.part_b) ? src.data.part_b.filter((w) => w.rust_cli && w.sf_cli) : [];
  if (b.length) parts.push(`${h}# Cold CLI, end to end`, '', coldCliTable(b, descriptions), '');
  return parts.join('\n');
}

/** Order: CI (Linux) sources first, default runner first, then local machines. */
function orderedSources(sources) {
  const rank = (s) => (s.kind === 'ci' ? (s.label.endsWith('ubuntu-latest') ? 0 : 1) : 2);
  return [...sources].sort((x, y) => rank(x) - rank(y) || x.label.localeCompare(y.label));
}

export function renderBenchmarkTables() {
  const descriptions = JSON.parse(fs.readFileSync(workloadsFile, 'utf8'));
  const [primary, ...others] = orderedSources(loadSources());
  const out = ['<!-- bench:tables:begin (generated by benchmarks/summarize.mjs; do not edit by hand) -->', ''];
  out.push(sourceSection(primary, descriptions, 3));
  for (const src of others) {
    out.push(`<details>\n<summary><strong>${esc(src.label)}</strong></summary>\n`, sourceSection(src, descriptions, 4), '</details>\n');
  }
  out.push('<!-- bench:tables:end -->');
  return out.join('\n');
}

/** Regenerates (write) or verifies (check) the generated blocks in docs/index.md and BENCHMARKS.md. */
export function syncOverview({ write = false } = {}) {
  const targets = [
    { file: overviewFile, block: BLOCK, name: 'bench:metrics', render: () => renderMetricsMarkdown(summarize()) },
    { file: benchmarksFile, block: TABLES, name: 'bench:tables', render: () => renderBenchmarkTables() },
  ];
  let changed = false;
  const stale = [];
  for (const t of targets) {
    const current = fs.readFileSync(t.file, 'utf8');
    if (!t.block.test(current)) {
      throw new Error(`${path.relative(repoRoot, t.file)} is missing the <!-- ${t.name}:begin --> ... <!-- ${t.name}:end --> markers.`);
    }
    const next = current.replace(t.block, () => t.render());
    if (next !== current) {
      changed = true;
      stale.push(path.relative(repoRoot, t.file));
      if (write) fs.writeFileSync(t.file, next);
    }
  }
  return { changed, stale };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.includes('--write-docs')) {
    const { changed } = syncOverview({ write: true });
    console.log(changed ? 'Generated blocks updated.' : 'Generated blocks already current.');
  } else if (args.includes('--check-docs')) {
    process.exit(syncOverview().changed ? 1 : 0);
  } else {
    console.log(JSON.stringify(summarize(), null, 2));
  }
}
