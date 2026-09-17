#!/usr/bin/env node
// Convergent VS Code Marketplace publication for platform-specific VSIX files.
//
// The gallery API can time out after it has accepted an upload, and `vsce
// publish` refuses a version that already exists. A plain loop therefore cannot
// be rerun after a partial publication. This script makes the end state the
// contract instead: it queries the Marketplace for the targets already live at
// the release version, publishes only the missing ones, retries a failed
// publish a bounded number of times (re-checking liveness before each retry,
// because a timed-out upload may have succeeded server-side), and fails only if
// some target is still missing afterwards, naming it.
//
// Usage (run from the extension directory so `npm exec` resolves vsce):
//   node ../.github/scripts/publish-vsix-convergent.mjs --version 0.7.0 --dir ../release-assets [--dry-run] [--attempts 3]
//   node .github/scripts/publish-vsix-convergent.mjs --self-test
import { readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export const EXTENSION = 'volute.cvc';
export const EXPECTED_TARGETS = Object.freeze(['linux-x64', 'darwin-x64', 'darwin-arm64', 'win32-x64']);
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const BACKOFF_SECONDS = Object.freeze([30, 60, 120]);

/** `vsce package --target` names files `<name>-<target>-<version>.vsix`. */
export function parseVsixName(fileName) {
  const match = /^(?<name>[a-z0-9-]+?)-(?<target>(?:linux|darwin|win32|alpine|web)-[a-z0-9]+)-(?<version>\d+\.\d+\.\d+)\.vsix$/.exec(fileName);
  if (!match) throw new Error(`Unrecognized VSIX file name ${JSON.stringify(fileName)}; expected <name>-<target>-<version>.vsix`);
  return { name: match.groups.name, target: match.groups.target, version: match.groups.version };
}

/** Targets of `version` that the Marketplace metadata already lists. */
export function liveTargets(metadata, version) {
  const versions = Array.isArray(metadata?.versions) ? metadata.versions : null;
  if (!versions) throw new Error('Marketplace metadata has no versions array');
  return new Set(versions.filter(entry => entry?.version === version && typeof entry.targetPlatform === 'string').map(entry => entry.targetPlatform));
}

export function missingTargets(live, wanted) {
  return wanted.filter(target => !live.has(target));
}

function parseArguments(argv) {
  const options = { version: null, dir: null, dryRun: false, attempts: 3, selfTest: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--self-test') options.selfTest = true;
    else if (argument === '--dry-run') options.dryRun = true;
    else if (argument === '--version') options.version = argv[++index];
    else if (argument === '--dir') options.dir = argv[++index];
    else if (argument === '--attempts') options.attempts = Number.parseInt(argv[++index], 10);
    else throw new Error(`Unknown argument ${JSON.stringify(argument)}`);
  }
  if (options.selfTest) return options;
  if (!options.version || !SEMVER.test(options.version)) throw new Error('--version must be an exact MAJOR.MINOR.PATCH');
  if (!options.dir) throw new Error('--dir <directory of .vsix files> is required');
  if (!Number.isInteger(options.attempts) || options.attempts < 1 || options.attempts > 5) throw new Error('--attempts must be 1..5');
  return options;
}

function vsce(args, { capture }) {
  const result = spawnSync('npm', ['exec', '--no', '--', 'vsce', ...args], {
    stdio: capture ? ['ignore', 'pipe', 'inherit'] : 'inherit',
    encoding: 'utf8',
    env: process.env,
  });
  if (result.error) throw result.error;
  return result;
}

function fetchLive(version) {
  const result = vsce(['show', EXTENSION, '--json'], { capture: true });
  if (result.status !== 0) throw new Error(`vsce show ${EXTENSION} failed with status ${result.status}`);
  return liveTargets(JSON.parse(result.stdout), version);
}

const sleep = seconds => new Promise(resolveSleep => setTimeout(resolveSleep, seconds * 1000));

async function publishAll(options) {
  const files = readdirSync(options.dir).filter(name => name.endsWith('.vsix')).sort();
  if (files.length === 0) throw new Error(`No .vsix files in ${options.dir}`);
  const packages = new Map();
  for (const file of files) {
    const parsed = parseVsixName(file);
    if (parsed.version !== options.version) throw new Error(`${file} is version ${parsed.version}, release is ${options.version}`);
    if (packages.has(parsed.target)) throw new Error(`Duplicate VSIX for target ${parsed.target}`);
    packages.set(parsed.target, resolve(options.dir, file));
  }
  const wanted = [...packages.keys()];
  const unexpected = wanted.filter(target => !EXPECTED_TARGETS.includes(target));
  if (unexpected.length > 0) throw new Error(`Unexpected targets: ${unexpected.join(', ')}`);

  let live = fetchLive(options.version);
  console.log(`Marketplace already has ${options.version} for: ${[...live].sort().join(', ') || '(no targets)'}`);
  for (const target of wanted) {
    if (live.has(target)) { console.log(`skip ${target}: already published`); continue; }
    for (let attempt = 1; attempt <= options.attempts; attempt += 1) {
      if (attempt > 1) {
        // A timed-out upload may have completed server-side; publishing it
        // again would fail on "version exists" and mask the real state.
        live = fetchLive(options.version);
        if (live.has(target)) { console.log(`${target}: became live after attempt ${attempt - 1}`); break; }
        const wait = BACKOFF_SECONDS[Math.min(attempt - 2, BACKOFF_SECONDS.length - 1)];
        console.log(`${target}: retry ${attempt}/${options.attempts} after ${wait}s`);
        await sleep(wait);
      }
      console.log(`publish ${target} (attempt ${attempt}/${options.attempts})${options.dryRun ? ' [dry run]' : ''}`);
      if (options.dryRun) { live.add(target); break; }
      const result = vsce(['publish', '--packagePath', packages.get(target)], { capture: false });
      if (result.status === 0) { live.add(target); break; }
      console.error(`${target}: vsce publish exited with status ${result.status}`);
    }
  }

  const finalLive = options.dryRun ? live : fetchLive(options.version);
  const missing = missingTargets(finalLive, wanted);
  if (missing.length > 0) {
    console.error(`Still missing on the Marketplace for ${options.version}: ${missing.join(', ')}. Rerun this job to converge, or follow the runbook's channel-specific recovery for exactly these targets.`);
    process.exit(1);
  }
  console.log(`All targets live for ${options.version}: ${wanted.join(', ')}`);
}

function selfTest() {
  const assert = (condition, message) => { if (!condition) throw new Error(`self-test failed: ${message}`); };
  const parsed = parseVsixName('cvc-win32-x64-0.7.0.vsix');
  assert(parsed.name === 'cvc' && parsed.target === 'win32-x64' && parsed.version === '0.7.0', 'parse platform VSIX name');
  assert(parseVsixName('cvc-darwin-arm64-1.2.3.vsix').target === 'darwin-arm64', 'parse arm target');
  let threw = false; try { parseVsixName('cvc-0.7.0.vsix'); } catch { threw = true; }
  assert(threw, 'reject a universal VSIX name');
  const metadata = { versions: [
    { version: '0.7.0', targetPlatform: 'linux-x64' },
    { version: '0.7.0', targetPlatform: 'darwin-x64' },
    { version: '0.6.0', targetPlatform: 'win32-x64' },
    { version: '0.7.0' },
  ] };
  const live = liveTargets(metadata, '0.7.0');
  assert(live.size === 2 && live.has('linux-x64') && live.has('darwin-x64'), 'live targets are version-scoped');
  const missing = missingTargets(live, EXPECTED_TARGETS);
  assert(missing.length === 2 && missing.includes('darwin-arm64') && missing.includes('win32-x64'), 'missing targets computed');
  threw = false; try { liveTargets({}, '0.7.0'); } catch { threw = true; }
  assert(threw, 'metadata without versions is rejected');
  console.log('publish-vsix-convergent self-tests passed.');
}

const options = parseArguments(process.argv.slice(2));
if (options.selfTest) selfTest();
else await publishAll(options);
