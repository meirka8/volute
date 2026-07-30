#!/usr/bin/env node
import { readFileSync } from 'node:fs';

const [tag] = process.argv.slice(2);
// Build metadata is deliberately excluded: npm and the installers distribute
// a single, unambiguous release version rather than SemVer build variants.
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$/;

if (!tag || !tag.startsWith('v') || !semver.test(tag.slice(1))) {
  throw new Error(`Release tag must be v<strict-semver>; received ${JSON.stringify(tag)}.`);
}

const expected = tag.slice(1);
const cargoManifests = [
  'cvc-core/Cargo.toml',
  'cvc-cli/Cargo.toml',
  'cvc-lsp/Cargo.toml',
  'cvc-mcp/Cargo.toml',
];

function cargoVersion(file) {
  const manifest = readFileSync(file, 'utf8');
  const matches = [...manifest.matchAll(/^version\s*=\s*"([^"]+)"\s*$/gm)];
  if (matches.length !== 1) throw new Error(`${file} must contain exactly one package version.`);
  return matches[0][1];
}

function packageVersion(file) {
  return JSON.parse(readFileSync(file, 'utf8')).version;
}

const versions = [
  ...cargoManifests.map(file => [file, cargoVersion(file)]),
  ['cvc-plugin/package.json', packageVersion('cvc-plugin/package.json')],
  ['npm/cvc-mcp/package.json', packageVersion('npm/cvc-mcp/package.json')],
];

const mismatches = versions.filter(([, version]) => version !== expected);
if (mismatches.length > 0) {
  throw new Error(`Release tag ${tag} does not match: ${mismatches.map(([file, version]) => `${file}=${version}`).join(', ')}.`);
}

console.log(`Release version gate passed: ${tag} matches all ${versions.length} package manifests.`);
