#!/usr/bin/env node
/** Deterministic, dependency-free release notice generator. Prerequisites: cargo
 * fetch --locked; (cd cvc-plugin && npm ci --ignore-scripts --omit=dev).
 * Limits: depth 8, 4,096 traversed entries/package, 128 evidence files/package,
 * 1 MiB/evidence file, and 32 MiB aggregate evidence. Limits fail closed. */
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, relative, resolve, sep } from 'node:path';

const root = realpathSync(resolve(dirname(new URL(import.meta.url).pathname), '../..'));
const output = resolve(root, 'THIRD-PARTY-NOTICES.md');
const check = process.argv[2] === '--check';
const selfTest = process.argv[2] === '--self-test';
if (process.argv.length !== ((check || selfTest) ? 3 : 2)) throw new Error('Usage: generate-third-party-notices.mjs [--check|--self-test]');
const LIMIT = Object.freeze({ depth: 8, entries: 4096, files: 128, fileBytes: 1024 * 1024, totalBytes: 32 * 1024 * 1024 });
const byteCompare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const normalise = text => text.replace(/\r\n?/g, '\n').replace(/\n*$/, '\n');
const sha256 = text => createHash('sha256').update(text).digest('hex');
const evidenceName = name => /^(?:licen[cs]e|copying|copyright|notices?|third[-_. ]?party[-_. ]?notices?|acknowledg(?:e)?ments?)(?:[._ -].*)?$/i.test(name);
const conventionalDirectory = name => /^(?:licen[cs]es|legal|notices?|acknowledg(?:e)?ments?)$/i.test(name);
const excludedDirectory = name => /^(?:node_modules|vendor|fixtures?|tests?)$/i.test(name);
const unknown = value => typeof value !== 'string' || !value.trim() || /^(unknown|unlicensed|see (?:license|licence))/i.test(value.trim());
const unsafe = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g;
const safe = value => {
  const text = typeof value === 'string' ? value : 'Not provided by package metadata';
  return text.replace(unsafe, char => `\\u${char.codePointAt(0).toString(16).padStart(4, '0')}`)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/\|/g, '&#124;').replace(/`/g, '&#96;')
    .replace(/\\/g, '&#92;').replace(/\[/g, '&#91;').replace(/\]/g, '&#93;').replace(/\(/g, '&#40;').replace(/\)/g, '&#41;').replace(/:/g, '&#58;');
};
function fenced(text) { const n = Math.max(0, ...[...text.matchAll(/`+/g)].map(x => x[0].length)); const f = '`'.repeat(Math.max(3, n + 1)); return [f + 'text', text, f]; }
function parseSpdx(input) {
  // Cargo metadata has historical slash-separated alternatives. Normalize only a
  // complete sequence of SPDX-shaped identifiers separated solely by slashes.
  const text = /^[A-Za-z0-9][A-Za-z0-9.-]*(?:\s*\/\s*[A-Za-z0-9][A-Za-z0-9.-]*)+$/.test(input)
    ? input.split('/').map(value => value.trim()).join(' OR ') : input;
  const tokens = []; let cursor = 0;
  while (cursor < text.length) {
    if (/\s/.test(text[cursor])) { cursor++; continue; }
    const match = /^(\(|\)|[A-Za-z0-9][A-Za-z0-9.-]*)/.exec(text.slice(cursor));
    if (!match) throw new Error(`malformed SPDX expression: ${JSON.stringify(input)}`);
    if (/^(?:AND|OR|WITH)[A-Za-z0-9.-]+$/.test(match[1])) throw new Error(`SPDX operator lacks a token boundary: ${input}`);
    tokens.push(match[1]); cursor += match[1].length;
  }
  let index = 0; const identifiers = new Set(); const peek = () => tokens[index];
  const primary = () => {
    if (peek() === '(') { index++; expression(); if (peek() !== ')') throw new Error(`unclosed SPDX parenthesis: ${input}`); index++; return; }
    const id = peek();
    if (!id || /^(AND|OR|WITH|\))$/.test(id)) throw new Error(`expected SPDX identifier: ${input}`);
    identifiers.add(id); index++;
    if (peek() === 'WITH') { index++; const exception = peek(); if (!exception || /^(AND|OR|WITH|\(|\))$/.test(exception)) throw new Error(`expected SPDX exception: ${input}`); identifiers.add(exception); index++; }
  };
  const conjunction = () => { primary(); while (peek() === 'AND') { index++; primary(); } };
  const expression = () => { conjunction(); while (peek() === 'OR') { index++; conjunction(); } };
  expression(); if (index !== tokens.length) throw new Error(`malformed SPDX operator sequence: ${input}`); return identifiers;
}
function contained(base, candidate, label) {
  const realBase = realpathSync(base); const realCandidate = realpathSync(candidate);
  if (!(realCandidate === realBase || realCandidate.startsWith(realBase + sep))) throw new Error(`${label}: escapes its package root`);
  return realCandidate;
}
function regular(path, label) { const stat = lstatSync(path); if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label}: symlink or non-regular file`); return stat; }
function directory(path, label) { const stat = lstatSync(path); if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`${label}: symlink or non-directory`); return contained(path, path, label); }

const canonical = new Map([
  ['MIT', ['MIT.txt', 'b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5']],
  ['Apache-2.0', ['Apache-2.0.txt', '074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff']],
  ['LGPL-2.1-or-later', ['LGPL-2.1-or-later.txt', '5749785c8bdefafcb5d798270ed0a967036fe2ca63dcedade1627565dfef81d2']],
  ['LLVM-exception', ['LLVM-exception.txt', 'e34c58338bd89d43e709e226610d8f32b3e3c47f4ad9a99a8dc1d4ac7842488e']],
]);
let aggregateBytes = 0;
function readEvidence(path, label) {
  const stat = regular(path, label);
  if (stat.size > LIMIT.fileBytes) throw new Error(`${label}: exceeds ${LIMIT.fileBytes} byte evidence limit`);
  aggregateBytes += stat.size;
  if (aggregateBytes > LIMIT.totalBytes) throw new Error(`evidence exceeds ${LIMIT.totalBytes} aggregate byte limit`);
  return normalise(readFileSync(path, 'utf8'));
}
function fallbackEvidence(pkg) {
  const ids = [...parseSpdx(pkg.license)];
  if (!ids.length || ids.some(id => !canonical.has(id))) throw new Error(`${pkg.ecosystem} ${pkg.name}@${pkg.version}: no canonical fallback for every SPDX identifier in ${pkg.license}`);
  return ids.map(id => {
    const [name, expected] = canonical.get(id); const path = resolve(root, '.github/licenses/spdx', name);
    const content = readEvidence(path, `canonical SPDX ${id}`);
    if (sha256(content) !== expected) throw new Error(`canonical SPDX ${id}: digest mismatch`);
    return { content, path: `canonical SPDX ${id}`, fallback: `Canonical SPDX ${id} text; package-provided author/contributor metadata: ${pkg.copyright || 'not provided'}` };
  });
}
function evidenceFor(pkg) {
  const packageRoot = directory(pkg.directory, `${pkg.name}: package root`); const files = []; let entries = 0;
  const visit = (current, depth, insideLegal) => {
    if (depth > LIMIT.depth) throw new Error(`${pkg.name}: evidence traversal exceeds depth ${LIMIT.depth}`);
    contained(packageRoot, current, `${pkg.name}: evidence directory`); directory(current, `${pkg.name}: evidence directory`);
    for (const entry of readdirSync(current, { withFileTypes: true }).sort((a, b) => byteCompare(a.name, b.name))) {
      if (++entries > LIMIT.entries) throw new Error(`${pkg.name}: evidence traversal exceeds ${LIMIT.entries} entries`);
      const child = resolve(current, entry.name); contained(packageRoot, child, `${pkg.name}: evidence entry`);
      const stat = lstatSync(child); if (stat.isSymbolicLink() || (!stat.isFile() && !stat.isDirectory())) throw new Error(`${pkg.name}: evidence entry is symlink or special file: ${entry.name}`);
      if (stat.isFile() && (insideLegal || evidenceName(entry.name))) { if (files.length >= LIMIT.files) throw new Error(`${pkg.name}: exceeds ${LIMIT.files} evidence files`); files.push(child); }
      if (stat.isDirectory() && !excludedDirectory(entry.name) && (insideLegal || conventionalDirectory(entry.name))) visit(child, depth + 1, true);
    }
  };
  visit(packageRoot, 0, false);
  if (!files.length) return fallbackEvidence(pkg);
  return files.sort(byteCompare).map(path => ({ content: readEvidence(path, `${pkg.name}: evidence file`), path: relative(packageRoot, path).split(sep).join('/') }));
}
if (selfTest) {
  for (const value of ['MIT', '(MIT OR Apache-2.0) AND Unicode-3.0', 'Apache-2.0 WITH LLVM-exception', 'MIT/Apache-2.0']) parseSpdx(value);
  for (const value of ['', 'MIT OR', 'AND MIT', 'MIT WITH', 'MIT OR OR Apache-2.0', '(MIT', 'MIT Apache-2.0', 'MIT;Apache-2.0', 'MIT ORApache-2.0']) { let failed = false; try { parseSpdx(value); } catch { failed = true; } if (!failed) throw new Error(`SPDX parser accepted malformed expression ${JSON.stringify(value)}`); }
  const temp = mkdtempSync('/tmp/cvc-notice-test-');
  try { mkdirSync(resolve(temp, 'LICENSES', 'component'), { recursive: true }); writeFileSync(resolve(temp, 'LICENSES', 'component', 'MIT.txt'), 'nested evidence\n'); const result = evidenceFor({ ecosystem: 'test', name: 'nested', version: '0', license: 'MIT', directory: temp }); if (result.length !== 1 || result[0].path !== 'LICENSES/component/MIT.txt') throw new Error('nested conventional evidence traversal failed'); } finally { rmSync(temp, { recursive: true, force: true }); }
  console.log('SPDX parser and nested evidence self-tests passed.'); process.exit(0);
}
function rustPackages() {
  const meta = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--format-version', '1'], { cwd: root, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 })); const workspace = new Set(meta.workspace_members);
  return meta.packages.filter(p => !workspace.has(p.id)).map(p => {
    if (unknown(p.license)) throw new Error(`Rust ${p.name}@${p.version}: missing or unknown SPDX license expression`); parseSpdx(p.license);
    const dir = dirname(p.manifest_path); if (!existsSync(dir)) throw new Error(`Rust ${p.name}@${p.version}: source unavailable; run cargo fetch --locked`);
    return { ecosystem: 'Rust', name: p.name, version: p.version, license: p.license, source: p.repository || p.homepage || p.source || null, directory: dir, copyright: p.authors?.join('; ') || null };
  });
}
function npmPackages() {
  const plugin = resolve(root, 'cvc-plugin'); const nodeRoot = directory(resolve(plugin, 'node_modules'), 'npm node_modules root'); const lock = JSON.parse(readFileSync(resolve(plugin, 'package-lock.json'), 'utf8'));
  const valid = /^node_modules\/(?:@[^/@\\.][^/@\\]*\/[^/@\\.][^/@\\]*|[^/@\\.][^/@\\]*)(?:\/node_modules\/(?:@[^/@\\.][^/@\\]*\/[^/@\\.][^/@\\]*|[^/@\\.][^/@\\]*))*$/;
  const entries = Object.entries(lock.packages).filter(([path, pkg]) => path && !pkg.dev); if (!entries.length) throw new Error('cvc-plugin/package-lock.json has no production packages');
  return entries.map(([path, locked]) => {
    if (typeof path !== 'string' || !valid.test(path) || path.includes('..') || path.includes('\\')) throw new Error(`npm lock path rejected: ${JSON.stringify(path)}`);
    const relativePath = path.slice('node_modules/'.length); const dir = resolve(nodeRoot, relativePath); contained(nodeRoot, dir, `npm ${path}`); directory(dir, `npm ${path}`);
    const manifest = resolve(dir, 'package.json'); contained(dir, manifest, `npm ${path} manifest`); regular(manifest, `npm ${path} manifest`);
    const installed = JSON.parse(readFileSync(manifest, 'utf8')); const expectedName = path.slice(path.lastIndexOf('node_modules/') + 'node_modules/'.length);
    if (installed.name !== expectedName || installed.version !== locked.version) throw new Error(`npm ${path}: installed package differs from lockfile`);
    const license = installed.license || locked.license; if (unknown(license)) throw new Error(`npm ${installed.name}@${installed.version}: missing or unknown SPDX license expression`); parseSpdx(license);
    const people = [installed.author, ...(installed.contributors || [])].filter(Boolean).map(x => typeof x === 'string' ? x : JSON.stringify(x)).join('; ');
    return { ecosystem: 'npm (production VSIX)', name: installed.name, version: installed.version, license, source: installed.repository?.url || installed.homepage || locked.resolved || null, directory: dir, copyright: people || null };
  });
}
const packages = [...rustPackages(), ...npmPackages()].sort((a, b) => byteCompare(`${a.ecosystem}\0${a.name}\0${a.version}`, `${b.ecosystem}\0${b.name}\0${b.version}`));
const evidence = new Map();
for (const pkg of packages) for (const item of evidenceFor(pkg)) { const hash = sha256(item.content); const group = evidence.get(hash) || { content: item.content, uses: [] }; group.uses.push({ package: `${pkg.ecosystem}: ${pkg.name}@${pkg.version}`, license: pkg.license, path: item.path, fallback: item.fallback || null }); evidence.set(hash, group); }
const lines = ['# Third-Party Notices', '', '<!-- Generated by .github/scripts/generate-third-party-notices.mjs; DO NOT EDIT. -->', '', `Coverage: ${packages.filter(p => p.ecosystem === 'Rust').length} Rust packages; ${packages.filter(p => p.ecosystem !== 'Rust').length} production npm packages; ${evidence.size} unique evidence texts.`, '', '## Package inventory', '', '| Ecosystem | Package | SPDX license expression | Source / repository |', '| --- | --- | --- | --- |', ...packages.map(p => `| ${safe(p.ecosystem)} | ${safe(`${p.name}@${p.version}`)} | ${safe(p.license)} | ${safe(p.source)} |`), '', '## License, copyright, and notice texts', ''];
for (const [hash, group] of [...evidence.entries()].sort((a, b) => byteCompare(a[0], b[0]))) { const uses = group.uses.sort((a, b) => byteCompare(`${a.package}\0${a.license}\0${a.path}`, `${b.package}\0${b.license}\0${b.path}`)); lines.push(`### Evidence SHA-256: ${hash}`, '', 'Exact package/file provenance:', ...uses.map(u => `- ${safe(u.package)} — ${safe(u.license)} — ${safe(u.path)}${u.fallback ? ` (${safe(u.fallback)})` : ''}`), '', ...fenced(group.content), ''); }
const rendered = `${lines.join('\n')}\n`;
if (check) { if (!existsSync(output) || readFileSync(output, 'utf8') !== rendered) throw new Error('THIRD-PARTY-NOTICES.md is stale; run generator'); console.log(`Third-party notices are current (${packages.length} packages, ${evidence.size} unique evidence texts).`); } else { writeFileSync(output, rendered); console.log(`Wrote THIRD-PARTY-NOTICES.md (${packages.length} packages, ${evidence.size} unique evidence texts).`); }
