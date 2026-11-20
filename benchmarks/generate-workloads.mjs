import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, '..');
const FIXTURES_DIR = path.join(REPO_ROOT, 'fixtures', 'sdr-snapshots');
const XML_HEADER = '<?xml version="1.0" encoding="UTF-8"?>\n';
const NPSP_REPO_URL = 'https://github.com/SalesforceFoundation/NPSP.git';
const NPSP_COMMIT = 'f5072b44781e7f31cb396c6919cfa7d0ead0ec6a';
const NPSP_CACHE_DIR = path.join(REPO_ROOT, 'benchmarks', '.cache', 'npsp');

const SNAPSHOT_ROUNDTRIP_FIXTURES = [
  'customObjects-and-children',
  'digitalExperienceBundle',
  'staticResource',
  'preset-decomposedPS2',
  'preset-decomposedESR',
];

function copyDirFiltered(src, dst) {
  fs.mkdirSync(dst, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    if (entry.name === '__snapshots__') {
      continue;
    }
    const sp = path.join(src, entry.name);
    const dp = path.join(dst, entry.name);
    if (entry.isDirectory()) {
      copyDirFiltered(sp, dp);
    } else {
      fs.copyFileSync(sp, dp);
    }
  }
}

function runRustCliConvert(cliBin, subcommand, inputDir, outDir, projectDir) {
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });
  execFileSync(
    cliBin,
    [
      'convert',
      subcommand,
      '--sourcepath',
      inputDir,
      '--output-dir',
      outDir,
      '--project-dir',
      projectDir,
      '--exact-dir',
    ],
    { stdio: 'pipe' }
  );
}

function buildSyntheticCustomObjectXml(index, fieldCount, ruleCount, listViewCount) {
  const objName = `BenchObj_${String(index).padStart(3, '0')}__c`;
  const parts = [
    XML_HEADER,
    '<CustomObject xmlns="http://soap.sforce.com/2006/04/metadata">\n',
    '    <deploymentStatus>Deployed</deploymentStatus>\n',
    `    <label>Bench Object ${index}</label>\n`,
    `    <pluralLabel>Bench Objects ${index}</pluralLabel>\n`,
    '    <nameField>\n',
    '        <label>Name</label>\n',
    '        <type>Text</type>\n',
    '    </nameField>\n',
    '    <sharingModel>ReadWrite</sharingModel>\n',
  ];

  for (let f = 0; f < fieldCount; f += 1) {
    const fieldName = `Field_${String(f).padStart(3, '0')}__c`;
    parts.push(
      '    <fields>\n',
      `        <fullName>${fieldName}</fullName>\n`,
      `        <label>Field ${f}</label>\n`,
      '        <length>80</length>\n',
      '        <required>false</required>\n',
      '        <type>Text</type>\n',
      '    </fields>\n'
    );
  }

  for (let r = 0; r < ruleCount; r += 1) {
    const ruleName = `Rule_${String(r).padStart(2, '0')}`;
    parts.push(
      '    <validationRules>\n',
      `        <fullName>${ruleName}</fullName>\n`,
      '        <active>true</active>\n',
      '        <errorConditionFormula>ISBLANK(Name)</errorConditionFormula>\n',
      `        <errorMessage>Validation rule ${r} failed</errorMessage>\n`,
      '    </validationRules>\n'
    );
  }

  for (let lv = 0; lv < listViewCount; lv += 1) {
    const lvName = `View_${String(lv).padStart(2, '0')}`;
    parts.push(
      '    <listViews>\n',
      `        <fullName>${lvName}</fullName>\n`,
      '        <filterScope>Everything</filterScope>\n',
      `        <label>View ${lv}</label>\n`,
      '    </listViews>\n'
    );
  }

  parts.push('</CustomObject>\n');
  return { fileName: `${objName}.object`, fullName: objName, xml: parts.join('') };
}

function buildSyntheticPackageXml(spec, objectNames, classNames, profileNames, permSetNames, lwcNames) {
  const sections = [
    ['ApexClass', classNames],
    ['CustomLabels', ['CustomLabels']],
    ['CustomObject', objectNames],
    ['LightningComponentBundle', lwcNames],
    ['PermissionSet', permSetNames],
    ['Profile', profileNames],
  ];
  const parts = [
    XML_HEADER,
    '<Package xmlns="http://soap.sforce.com/2006/04/metadata">\n',
  ];
  for (const [typeName, members] of sections) {
    parts.push('    <types>\n');
    for (const m of members) {
      parts.push(`        <members>${m}</members>\n`);
    }
    parts.push(`        <name>${typeName}</name>\n`, '    </types>\n');
  }
  parts.push(`    <version>${spec.apiVersion ?? '63.0'}</version>\n`, '</Package>\n');
  return parts.join('');
}

function writeSyntheticOrg(cliBin, rootDir, spec) {
  const projectDir = path.join(rootDir, spec.name);
  const mdapiDir = path.join(projectDir, 'originalMdapi');
  const sfdxDir = path.join(projectDir, 'force-app');

  fs.mkdirSync(mdapiDir, { recursive: true });
  fs.writeFileSync(
    path.join(projectDir, 'sfdx-project.json'),
    JSON.stringify(
      {
        packageDirectories: [{ path: 'force-app', default: true }],
        namespace: '',
        sfdcLoginUrl: 'https://login.salesforce.com',
        sourceApiVersion: '63.0',
      },
      null,
      2
    )
  );

  // 1. CustomObjects with fields, validationRules, listViews
  const objectsDir = path.join(mdapiDir, 'objects');
  fs.mkdirSync(objectsDir, { recursive: true });
  const objectNames = [];
  for (let i = 0; i < spec.objects; i += 1) {
    const obj = buildSyntheticCustomObjectXml(i, spec.fieldsPerObject, spec.rulesPerObject, spec.listViewsPerObject);
    objectNames.push(obj.fullName);
    fs.writeFileSync(path.join(objectsDir, obj.fileName), obj.xml);
  }

  // 2. ApexClasses
  const classesDir = path.join(mdapiDir, 'classes');
  fs.mkdirSync(classesDir, { recursive: true });
  const classNames = [];
  for (let i = 0; i < spec.classes; i += 1) {
    const clsName = `BenchClass_${String(i).padStart(4, '0')}`;
    classNames.push(clsName);
    fs.writeFileSync(
      path.join(classesDir, `${clsName}.cls`),
      `public with sharing class ${clsName} {\n    public static Integer getValue() { return ${i}; }\n}\n`
    );
    fs.writeFileSync(
      path.join(classesDir, `${clsName}.cls-meta.xml`),
      `${XML_HEADER}<ApexClass xmlns="http://soap.sforce.com/2006/04/metadata">\n    <apiVersion>63.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n`
    );
  }

  // 3. Profiles
  const profilesDir = path.join(mdapiDir, 'profiles');
  fs.mkdirSync(profilesDir, { recursive: true });
  const profileNames = [];
  for (let i = 0; i < spec.profiles; i += 1) {
    const pName = `BenchProfile_${String(i).padStart(2, '0')}`;
    profileNames.push(pName);
    const lines = [
      XML_HEADER,
      '<Profile xmlns="http://soap.sforce.com/2006/04/metadata">\n',
      '    <custom>true</custom>\n',
    ];
    for (let c = 0; c < Math.min(10, classNames.length); c += 1) {
      lines.push(
        '    <classAccesses>\n',
        `        <apexClass>${classNames[c]}</apexClass>\n`,
        '        <enabled>true</enabled>\n',
        '    </classAccesses>\n'
      );
    }
    for (let o = 0; o < Math.min(5, objectNames.length); o += 1) {
      lines.push(
        '    <objectPermissions>\n',
        '        <allowCreate>true</allowCreate>\n',
        '        <allowDelete>false</allowDelete>\n',
        '        <allowEdit>true</allowEdit>\n',
        '        <allowRead>true</allowRead>\n',
        '        <modifyAllRecords>false</modifyAllRecords>\n',
        `        <object>${objectNames[o]}</object>\n`,
        '        <viewAllRecords>false</viewAllRecords>\n',
        '    </objectPermissions>\n'
      );
    }
    lines.push('</Profile>\n');
    fs.writeFileSync(path.join(profilesDir, `${pName}.profile`), lines.join(''));
  }

  // 4. PermissionSets
  const permSetsDir = path.join(mdapiDir, 'permissionsets');
  fs.mkdirSync(permSetsDir, { recursive: true });
  const permSetNames = [];
  for (let i = 0; i < spec.permSets; i += 1) {
    const psName = `BenchPermSet_${String(i).padStart(2, '0')}`;
    permSetNames.push(psName);
    const lines = [
      XML_HEADER,
      '<PermissionSet xmlns="http://soap.sforce.com/2006/04/metadata">\n',
      `    <label>Bench PermSet ${i}</label>\n`,
    ];
    for (let c = 0; c < Math.min(8, classNames.length); c += 1) {
      lines.push(
        '    <classAccesses>\n',
        `        <apexClass>${classNames[c]}</apexClass>\n`,
        '        <enabled>true</enabled>\n',
        '    </classAccesses>\n'
      );
    }
    lines.push('</PermissionSet>\n');
    fs.writeFileSync(path.join(permSetsDir, `${psName}.permissionset`), lines.join(''));
  }

  // 5. CustomLabels
  const labelsDir = path.join(mdapiDir, 'labels');
  fs.mkdirSync(labelsDir, { recursive: true });
  const labelLines = [XML_HEADER, '<CustomLabels xmlns="http://soap.sforce.com/2006/04/metadata">\n'];
  for (let i = 0; i < spec.labels; i += 1) {
    const lblName = `Bench_Label_${String(i).padStart(4, '0')}`;
    labelLines.push(
      '    <labels>\n',
      `        <fullName>${lblName}</fullName>\n`,
      '        <language>en_US</language>\n',
      '        <protected>true</protected>\n',
      `        <shortDescription>Label ${i}</shortDescription>\n`,
      `        <value>Benchmark value ${i}</value>\n`,
      '    </labels>\n'
    );
  }
  labelLines.push('</CustomLabels>\n');
  fs.writeFileSync(path.join(labelsDir, 'CustomLabels.labels'), labelLines.join(''));

  // 6. LightningComponentBundles (4 files per bundle)
  const lwcRoot = path.join(mdapiDir, 'lwc');
  fs.mkdirSync(lwcRoot, { recursive: true });
  const lwcNames = [];
  for (let i = 0; i < spec.lwcs; i += 1) {
    const lwcName = `benchLwc${String(i).padStart(3, '0')}`;
    lwcNames.push(lwcName);
    const bundleDir = path.join(lwcRoot, lwcName);
    fs.mkdirSync(bundleDir, { recursive: true });
    fs.writeFileSync(
      path.join(bundleDir, `${lwcName}.html`),
      `<template><div class="box">${lwcName}</div></template>\n`
    );
    fs.writeFileSync(
      path.join(bundleDir, `${lwcName}.js`),
      `import { LightningElement } from 'lwc';\nexport default class extends LightningElement {}\n`
    );
    fs.writeFileSync(path.join(bundleDir, `${lwcName}.css`), `.box { color: #333; }\n`);
    fs.writeFileSync(
      path.join(bundleDir, `${lwcName}.js-meta.xml`),
      `${XML_HEADER}<LightningComponentBundle xmlns="http://soap.sforce.com/2006/04/metadata">\n    <apiVersion>63.0</apiVersion>\n    <isExposed>false</isExposed>\n</LightningComponentBundle>\n`
    );
  }

  fs.writeFileSync(
    path.join(mdapiDir, 'package.xml'),
    buildSyntheticPackageXml(spec, objectNames, classNames, profileNames, permSetNames, lwcNames)
  );

  // Prepare paired SFDX source directory once outside any benchmark timer
  runRustCliConvert(cliBin, 'source', mdapiDir, sfdxDir, projectDir);

  return { name: spec.name, projectDir, mdapiDir, sfdxDir };
}

function ensureNpspCache(cacheDir) {
  if (fs.existsSync(path.join(cacheDir, '.git'))) {
    try {
      const head = execFileSync('git', ['-C', cacheDir, 'rev-parse', 'HEAD'], {
        stdio: 'pipe',
        encoding: 'utf8',
      }).trim();
      if (head === NPSP_COMMIT) {
        return;
      }
    } catch {
      // Fall through to clean re-fetch if cached git metadata is incomplete.
    }
  }

  fs.rmSync(cacheDir, { recursive: true, force: true });
  fs.mkdirSync(cacheDir, { recursive: true });
  execFileSync('git', ['init'], { cwd: cacheDir, stdio: 'pipe' });
  execFileSync('git', ['remote', 'add', 'origin', NPSP_REPO_URL], {
    cwd: cacheDir,
    stdio: 'pipe',
  });
  execFileSync('git', ['fetch', '--depth', '1', 'origin', NPSP_COMMIT], {
    cwd: cacheDir,
    stdio: 'pipe',
  });
  execFileSync('git', ['checkout', '--detach', 'FETCH_HEAD'], {
    cwd: cacheDir,
    stdio: 'pipe',
  });
}

function prepareNpspWorkload(cliBin, outRoot) {
  ensureNpspCache(NPSP_CACHE_DIR);
  const projDir = path.join(outRoot, 'real-world', 'npsp');
  fs.mkdirSync(projDir, { recursive: true });

  for (const fileName of ['sfdx-project.json', '.forceignore']) {
    const srcFile = path.join(NPSP_CACHE_DIR, fileName);
    if (fs.existsSync(srcFile)) {
      fs.copyFileSync(srcFile, path.join(projDir, fileName));
    }
  }

  const sfdxDir = path.join(projDir, 'force-app');
  copyDirFiltered(path.join(NPSP_CACHE_DIR, 'force-app'), sfdxDir);

  const mdapiDir = path.join(projDir, 'originalMdapi');
  runRustCliConvert(cliBin, 'mdapi', sfdxDir, mdapiDir, projDir);

  return {
    name: 'npsp',
    commit: NPSP_COMMIT,
    projectDir: projDir,
    mdapiDir,
    sfdxDir,
  };
}

function main() {
  const outRoot = process.argv[2];
  const cliBin =
    process.argv[3] ?? path.join(REPO_ROOT, 'target', 'release', 'sf-source-format');
  if (!outRoot) {
    throw new Error('Usage: node generate-workloads.mjs <out-root> [sf-source-format-bin]');
  }
  fs.mkdirSync(outRoot, { recursive: true });

  const fixturesOut = {};
  const fixturesBase = path.join(outRoot, 'fixtures');
  for (const name of SNAPSHOT_ROUNDTRIP_FIXTURES) {
    const projDir = path.join(fixturesBase, name);
    copyDirFiltered(path.join(FIXTURES_DIR, name), projDir);
    const mdapiDir = path.join(projDir, 'originalMdapi');
    const sfdxDir = path.join(projDir, 'force-app');
    runRustCliConvert(cliBin, 'source', mdapiDir, sfdxDir, projDir);
    fixturesOut[name] = { name, projectDir: projDir, mdapiDir, sfdxDir };
  }

  const labelsName = 'customLabels-multiple';
  const labelsProj = path.join(fixturesBase, labelsName);
  copyDirFiltered(path.join(FIXTURES_DIR, labelsName), labelsProj);
  fixturesOut[labelsName] = {
    name: labelsName,
    projectDir: labelsProj,
    mdapiDir: null,
    sfdxDir: path.join(labelsProj, 'originalSource'),
  };

  const syntheticBase = path.join(outRoot, 'synthetic');
  const syntheticSpecs = [
    {
      name: 'medium-org',
      objects: 25,
      fieldsPerObject: 40,
      rulesPerObject: 8,
      listViewsPerObject: 4,
      classes: 100,
      profiles: 15,
      permSets: 10,
      labels: 500,
      lwcs: 10,
    },
    {
      name: 'large-org',
      objects: 100,
      fieldsPerObject: 50,
      rulesPerObject: 10,
      listViewsPerObject: 4,
      classes: 400,
      profiles: 30,
      permSets: 20,
      labels: 2000,
      lwcs: 25,
    },
  ];
  const syntheticOut = {};
  for (const spec of syntheticSpecs) {
    syntheticOut[spec.name] = writeSyntheticOrg(cliBin, syntheticBase, spec);
  }

  const realWorldOut = {
    npsp: prepareNpspWorkload(cliBin, outRoot),
  };

  process.stdout.write(
    `${JSON.stringify({ fixtures: fixturesOut, synthetic: syntheticOut, realWorld: realWorldOut }, null, 2)}\n`
  );
}

main();
