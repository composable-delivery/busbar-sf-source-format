import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import {
  ComponentSetBuilder,
  MetadataConverter,
  RegistryAccess,
} from '@salesforce/source-deploy-retrieve';

function countFilesRecursive(dir) {
  let total = 0;
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return 0;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      total += countFilesRecursive(full);
    } else {
      total += 1;
    }
  }
  return total;
}

/**
 * Counts non-directory entries in a ZIP buffer by scanning the Central Directory
 * from the End-of-Central-Directory (EOCD) record, so directory-only records are
 * excluded cleanly for an apples-to-apples count with Rust.
 */
function countZipFileEntries(buf) {
  const minOffset = Math.max(0, buf.length - 65557);
  let eocdOffset = -1;
  for (let i = buf.length - 22; i >= minOffset; i -= 1) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocdOffset = i;
      break;
    }
  }
  if (eocdOffset < 0) {
    throw new Error('ZIP End-of-Central-Directory record not found');
  }
  const cdSize = buf.readUInt32LE(eocdOffset + 12);
  const cdOffset = buf.readUInt32LE(eocdOffset + 16);
  let offset = cdOffset;
  const cdEnd = cdOffset + cdSize;
  let fileCount = 0;
  while (offset < cdEnd) {
    if (buf.readUInt32LE(offset) !== 0x02014b50) {
      throw new Error(`Invalid ZIP central directory header at ${offset}`);
    }
    const nameLen = buf.readUInt16LE(offset + 28);
    const extraLen = buf.readUInt16LE(offset + 30);
    const commentLen = buf.readUInt16LE(offset + 32);
    const fileName = buf.subarray(offset + 46, offset + 46 + nameLen).toString('utf8');
    if (!fileName.endsWith('/')) {
      fileCount += 1;
    }
    offset += 46 + nameLen + extraLen + commentLen;
  }
  return fileCount;
}

async function runSingleIteration(spec, tag) {
  const targetFormat = spec.direction === 'mdapi_to_source' ? 'source' : 'metadata';
  const outDir = path.join(spec.output_base_dir, tag);

  if (spec.mode === 'directory') {
    fs.rmSync(outDir, { recursive: true, force: true });
    fs.mkdirSync(outDir, { recursive: true });
  }

  const outputConfig =
    spec.mode === 'directory'
      ? {
          type: 'directory',
          outputDirectory: outDir,
          genUniqueDir: false,
        }
      : {
          type: 'zip',
        };

  const prevCwd = process.cwd();
  if (spec.project_dir) {
    process.chdir(spec.project_dir);
  }

  let elapsedMs = 0;
  let convertRes;
  try {
    const startNs = process.hrtime.bigint();
    const registry = spec.project_dir
      ? new RegistryAccess(undefined, spec.project_dir)
      : new RegistryAccess();
    const cs = await ComponentSetBuilder.build({
      sourcepath: [spec.input_dir],
      projectDir: spec.project_dir ?? undefined,
    });
    convertRes = await new MetadataConverter(registry).convert(cs, targetFormat, outputConfig);
    const endNs = process.hrtime.bigint();
    elapsedMs = Number(endNs - startNs) / 1_000_000;
  } finally {
    process.chdir(prevCwd);
  }

  let outputCount = 0;
  if (spec.mode === 'directory') {
    outputCount = countFilesRecursive(outDir);
    fs.rmSync(outDir, { recursive: true, force: true });
  } else {
    if (!convertRes.zipBuffer) {
      throw new Error(`Missing zipBuffer for ${spec.name}`);
    }
    outputCount = countZipFileEntries(convertRes.zipBuffer);
  }

  return { elapsedMs, outputCount };
}

async function main() {
  const manifestPath = process.argv[2];
  if (!manifestPath) {
    throw new Error('Usage: node bench.mjs <manifest.json>');
  }
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const results = [];

  for (const spec of manifest.workloads) {
    for (let i = 0; i < spec.warmup; i += 1) {
      await runSingleIteration(spec, `warmup_${i}`);
    }
    const durationsMs = [];
    let outputCount = 0;
    for (let i = 0; i < spec.iterations; i += 1) {
      const { elapsedMs, outputCount: count } = await runSingleIteration(spec, `iter_${i}`);
      durationsMs.push(elapsedMs);
      outputCount = count;
    }
    results.push({
      name: spec.name,
      durations_ms: durationsMs,
      output_count: outputCount,
    });
  }

  process.stdout.write(`${JSON.stringify({ engine: 'node-sdr', results }, null, 2)}\n`);
}

main().catch((err) => {
  process.stderr.write(`${err.stack ?? err.message ?? String(err)}\n`);
  process.exit(1);
});
