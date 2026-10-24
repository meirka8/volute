#!/usr/bin/env node
import { readFileSync } from 'node:fs';

const [tag] = process.argv.slice(2);
// Publishing uses npm's latest tag and stable VSCE releases, so accept only
// exact stable SemVer versions: no prerelease identifiers or build metadata.
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

if (!tag || !tag.startsWith('v') || !semver.test(tag.slice(1))) {
  throw new Error(`Release tag must be v<MAJOR.MINOR.PATCH> with no prerelease or build metadata; received ${JSON.stringify(tag)}.`);
}

const expected = tag.slice(1);
const cargoManifests = [
  'cvc-core/Cargo.toml',
  'cvc-cli/Cargo.toml',
  'cvc-lsp/Cargo.toml',
  'cvc-mcp/Cargo.toml',
];

const claudePluginManifest = 'plugins/claude-code/.claude-plugin/plugin.json';
const claudeMarketplace = '.claude-plugin/marketplace.json';

function cargoVersion(file) {
  const manifest = readFileSync(file, 'utf8');
  const matches = [...manifest.matchAll(/^version\s*=\s*"([^"]+)"\s*$/gm)];
  if (matches.length !== 1) throw new Error(`${file} must contain exactly one package version.`);
  return matches[0][1];
}

function packageVersion(file) {
  return JSON.parse(readFileSync(file, 'utf8')).version;
}

// The Claude Code plugin marketplace pins the `cvc` plugin to a release tag with a
// git-subdir `ref`, so only a release publishes plugin content. The ref is the tag itself.
function marketplaceRef(file) {
  const entry = JSON.parse(readFileSync(file, 'utf8')).plugins?.find(plugin => plugin?.name === 'cvc');
  return entry?.source?.ref;
}

const versions = [
  ...cargoManifests.map(file => [file, cargoVersion(file)]),
  ['cvc-plugin/package.json', packageVersion('cvc-plugin/package.json')],
  ['npm/cvc-mcp/package.json', packageVersion('npm/cvc-mcp/package.json')],
  // The Claude Code plugin's manifest version is what Claude Code compares to
  // decide whether installed copies need updating, so it must move with the CLI.
  [claudePluginManifest, packageVersion(claudePluginManifest)],
];

const refs = [[`${claudeMarketplace} (cvc source.ref)`, marketplaceRef(claudeMarketplace)]];

const mismatches = [
  ...versions.filter(([, version]) => version !== expected),
  ...refs.filter(([, ref]) => ref !== tag),
];
if (mismatches.length > 0) {
  throw new Error(`Release tag ${tag} does not match: ${mismatches.map(([file, version]) => `${file}=${version}`).join(', ')}.`);
}

console.log(`Release version gate passed: ${tag} matches all ${versions.length + refs.length} release manifests.`);
