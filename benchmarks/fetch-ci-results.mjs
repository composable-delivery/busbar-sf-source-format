#!/usr/bin/env node
// Downloads the cross-engine comparison results produced by the `Benchmarks` workflow
// (artifact `benchmark-comparison-results-<runner>`) into benchmarks/results/ci/<runner>.json.
//
// Usage: node benchmarks/fetch-ci-results.mjs [--run <run-id>]
// Requires the GitHub CLI (`gh`) to be authenticated. Re-run `node benchmarks/summarize.mjs`
// (or the docs build) afterwards to refresh the Overview metrics.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = path.join(repoRoot, 'benchmarks', 'results', 'ci');
const gh = (...args) => execFileSync('gh', args, { cwd: repoRoot, encoding: 'utf8' });

const repo = JSON.parse(gh('repo', 'view', '--json', 'nameWithOwner')).nameWithOwner;
const flag = process.argv.indexOf('--run');
let runId = flag > -1 ? process.argv[flag + 1] : null;

const runs = JSON.parse(
  gh('run', 'list', '--workflow', 'bench.yml', '--status', 'success', '--limit', '30',
    '--json', 'databaseId,headSha,createdAt,event,url')
);
const hasComparison = (id) =>
  JSON.parse(gh('api', `repos/${repo}/actions/runs/${id}/artifacts`, '--jq', '{a:[.artifacts[].name]}'))
    .a.some((n) => n.startsWith('benchmark-comparison-results-'));

const run = runId
  ? runs.find((r) => String(r.databaseId) === String(runId)) ?? { databaseId: runId }
  : runs.find((r) => hasComparison(r.databaseId));
if (!run) {
  console.error('No successful Benchmarks run with comparison artifacts found. Dispatch one with:\n' +
    '  gh workflow run bench.yml -f suite=comparison -f runners=cross-os');
  process.exit(1);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'bench-ci-'));
gh('run', 'download', String(run.databaseId), '--pattern', 'benchmark-comparison-results-*', '--dir', tmp);
fs.mkdirSync(outDir, { recursive: true });
for (const dir of fs.readdirSync(tmp)) {
  const runner = dir.replace('benchmark-comparison-results-', '');
  const data = JSON.parse(fs.readFileSync(path.join(tmp, dir, 'results.json'), 'utf8'));
  data.source = { kind: 'ci', runner, runId: run.databaseId, sha: run.headSha, date: run.createdAt, url: run.url };
  fs.writeFileSync(path.join(outDir, `${runner}.json`), `${JSON.stringify(data, null, 2)}\n`);
  console.log(`[bench] ${runner} <- run ${run.databaseId}`);
}
fs.rmSync(tmp, { recursive: true, force: true });
