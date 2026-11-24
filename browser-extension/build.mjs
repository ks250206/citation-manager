#!/usr/bin/env node
// Build for both browsers from one source (esbuild + the manifest generator in src/manifest.ts).
//
//   node build.mjs              dist/chrome, dist/firefox (development IDs)
//   node build.mjs --release    same with the release IDs from native-manifests/extension-ids.json
//   node build.mjs --tests      bundle tests/unit/*.test.ts into dist/tests for `node --test`
//   node build.mjs --check      static policy checks of the sources and built manifests
import { build } from 'esbuild';
import { cpSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const args = new Set(process.argv.slice(2));
const dist = join(root, 'dist');
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
const ids = JSON.parse(readFileSync(join(root, 'native-manifests/extension-ids.json'), 'utf8'));

const common = { bundle: true, target: ['chrome116', 'firefox128'], legalComments: 'none', logLevel: 'warning', charset: 'utf8' };

async function loadManifestModule() {
  const out = join(dist, '.tools/manifest.mjs');
  await build({ ...common, entryPoints: [join(root, 'src/manifest.ts')], outfile: out, format: 'esm', platform: 'node' });
  return import(pathToFileURL(out).href);
}

async function buildExtension(channel) {
  const { buildManifest } = await loadManifestModule();
  for (const target of ['chrome', 'firefox']) {
    const out = join(dist, target);
    rmSync(out, { recursive: true, force: true });
    mkdirSync(out, { recursive: true });
    await build({ ...common, entryPoints: [join(root, 'src/background/index.ts')], outfile: join(out, 'background.js'), format: 'iife' });
    await build({ ...common, entryPoints: [join(root, 'src/popup/index.ts')], outfile: join(out, 'popup.js'), format: 'iife' });
    // The snapshot is the completion value of content.js (see src/content/index.ts).
    await build({
      ...common,
      entryPoints: [join(root, 'src/content/index.ts')],
      outfile: join(out, 'content.js'),
      format: 'iife',
      globalName: '__cmCapture',
      footer: { js: '__cmCapture.run();' },
    });
    cpSync(join(root, 'public'), out, { recursive: true });
    const manifest = buildManifest(target, channel, pkg.version, ids);
    writeFileSync(join(out, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    console.log(`built ${target} (${channel}) -> ${out}`);
  }
}

async function buildTests() {
  const dir = join(root, 'tests/unit');
  const out = join(dist, 'tests');
  rmSync(out, { recursive: true, force: true });
  const entries = readdirSync(dir).filter((f) => f.endsWith('.test.ts')).map((f) => join(dir, f));
  await build({
    ...common,
    target: 'node22',
    entryPoints: entries,
    outdir: out,
    outExtension: { '.js': '.mjs' },
    format: 'esm',
    platform: 'node',
    packages: 'external',
    define: { __REPO_ROOT__: JSON.stringify(join(root, '..')) },
  });
  console.log(`built ${entries.length} test files -> ${out}`);
}

function walk(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)]));
}

async function check() {
  const problems = [];
  // Page and host strings must never become markup or code.
  for (const file of walk(join(root, 'src')).filter((f) => f.endsWith('.ts'))) {
    const text = readFileSync(file, 'utf8');
    for (const [pattern, why] of [
      [/\.innerHTML\s*=|\.outerHTML\s*=|insertAdjacentHTML|document\.write/, 'markup sink'],
      [/\beval\s*\(|new Function\s*\(|setTimeout\(\s*['"`]/, 'dynamic code'],
      [/<all_urls>|host_permissions|content_scripts/, 'broad page access'],
    ]) {
      if (pattern.test(text) && !file.endsWith('manifest.ts')) problems.push(`${file}: ${why}`);
    }
  }
  const { buildManifest, PERMISSIONS } = await loadManifestModule();
  for (const target of ['chrome', 'firefox']) {
    const m = buildManifest(target, 'development', pkg.version, ids);
    const text = JSON.stringify(m);
    if (m.manifest_version !== 3) problems.push(`${target}: not MV3`);
    for (const key of ['host_permissions', 'optional_host_permissions', 'content_scripts', 'web_accessible_resources', 'externally_connectable']) {
      if (key in m) problems.push(`${target}: ${key} present`);
    }
    if (text.includes('<all_urls>') || /\*:\/\//.test(text)) problems.push(`${target}: URL pattern in manifest`);
    if (JSON.stringify(m.permissions) !== JSON.stringify(PERMISSIONS)) problems.push(`${target}: unexpected permissions`);
  }
  for (const icon of ['16', '32', '48', '128']) {
    if (!existsSync(join(root, `public/icons/icon-${icon}.png`))) problems.push(`missing icon ${icon}`);
  }
  if (problems.length) {
    console.error(problems.join('\n'));
    process.exit(1);
  }
  console.log('policy checks passed');
}

if (args.has('--tests')) await buildTests();
else if (args.has('--check')) await check();
else await buildExtension(args.has('--release') ? 'release' : 'development');
