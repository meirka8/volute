#!/usr/bin/env node
// Convergent VS Code Marketplace publication for platform-specific VSIX files.
//
// The gallery API can time out after it has accepted an upload, and `vsce
// publish` refuses a version that already exists. A plain loop therefore cannot
// be rerun after a partial publication. This script makes the end state the
// contract instead: it queries the Marketplace for the targets already live at
// the release version, publishes only the missing ones, retries a failed
// publish a bounded number of times (re-checking liveness before each retry,
// because a timed-out upload may have succeeded server-side), and then waits a
// bounded time for the listing to show every target.
//
// That wait is part of the contract because the gallery validates and indexes an
// upload asynchronously (`vsce publish` itself says the extension URL "might
// take a few minutes"): a target whose publish exited 0 is accepted, not yet
// necessarily listed. After the publish loop the script polls `vsce show` every
// LISTING_POLL_SECONDS (the first poll is immediate) until all targets are
// listed or LISTING_DEADLINE_SECONDS have passed on a clock. A poll that fails
// is logged and skipped, never fatal. It fails only if a target is still not
// listed at the deadline, and then names two groups separately: targets the
// Marketplace accepted but has not listed (indexing delay; rerun once they are
// listed) and targets whose every publish attempt failed (a real failure).
// A dry run never polls.
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
// How often, and for how long after the last upload, to wait for the gallery to
// list accepted targets. Indexing has been observed to take about four minutes.
export const LISTING_POLL_SECONDS = 30;
export const LISTING_DEADLINE_SECONDS = 10 * 60;

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

/** Validates the VSIX file names found in `dir` and maps each target to its path. */
export function validatePackages(dir, version, fileNames) {
  if (fileNames.length === 0) throw new Error(`No .vsix files in ${dir}`);
  const packages = new Map();
  for (const file of fileNames) {
    const parsed = parseVsixName(file);
    if (parsed.version !== version) throw new Error(`${file} is version ${parsed.version}, release is ${version}`);
    if (packages.has(parsed.target)) throw new Error(`Duplicate VSIX for target ${parsed.target}`);
    packages.set(parsed.target, resolve(dir, file));
  }
  const unexpected = [...packages.keys()].filter(target => !EXPECTED_TARGETS.includes(target));
  if (unexpected.length > 0) throw new Error(`Unexpected targets: ${unexpected.join(', ')}`);
  return packages;
}

/** Reads `dir` and returns the validated target -> VSIX path map. */
export function discoverPackages(dir, version) {
  const fileNames = readdirSync(dir).filter(name => name.endsWith('.vsix')).sort();
  return validatePackages(dir, version, fileNames);
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

// Everything convergePublication touches outside itself, so the self-test can
// replace the network, vsce, the clock and the output. `publish` returns vsce's
// exit status; `now` returns monotonic milliseconds.
const REAL_DEPS = Object.freeze({
  fetchLive,
  publish: (target, packagePath) => vsce(['publish', '--packagePath', packagePath], { capture: false }).status,
  sleep,
  now: () => performance.now(),
  log: message => console.log(message),
  warn: message => console.error(message),
});

/**
 * Publishes the targets of `packages` (a target -> VSIX path Map) that are not
 * live yet, then waits for the listing to show all of them. Returns the outcome
 * instead of exiting: `acceptedMissing` are targets whose publish exited 0 but
 * that are still unlisted at the deadline, `failedMissing` are unlisted targets
 * for which no publish ever exited 0. Only the pre-publish listing read may
 * throw; later read failures are logged and the last known listing is kept.
 */
export async function convergePublication(config, overrides = {}) {
  const { version, packages, dryRun = false, attempts = 3, pollSeconds = LISTING_POLL_SECONDS, deadlineSeconds = LISTING_DEADLINE_SECONDS } = config;
  if (!(pollSeconds > 0)) throw new RangeError('pollSeconds must be positive');
  const { fetchLive, publish, sleep, now, log, warn } = { ...REAL_DEPS, ...overrides };
  const wanted = [...packages.keys()];
  const sorted = targets => [...targets].sort().join(', ') || '(none)';

  const live = new Set(await fetchLive(version));
  log(`Marketplace already has ${version} for: ${sorted(live)}`);

  // Folds a fresh read into `live`. A target seen listed stays listed (nothing
  // here can unpublish one), so a stale or partial read cannot un-list it. A
  // failed read keeps the last known state: it is a gallery or network blip,
  // not a reason to abandon a publication that may already have succeeded.
  const refresh = async context => {
    try {
      for (const target of await fetchLive(version)) live.add(target);
      return true;
    } catch (error) {
      warn(`${context}: could not read the Marketplace listing (${error.message}); keeping the last known state`);
      return false;
    }
  };

  const accepted = new Set();
  for (const target of wanted) {
    if (live.has(target)) { log(`skip ${target}: already published`); continue; }
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      if (attempt > 1) {
        // A timed-out upload may have completed server-side; publishing it
        // again would fail on "version exists" and mask the real state.
        await refresh(`${target}: before retry ${attempt}`);
        if (live.has(target)) { log(`${target}: became live after attempt ${attempt - 1}`); break; }
        const wait = BACKOFF_SECONDS[Math.min(attempt - 2, BACKOFF_SECONDS.length - 1)];
        log(`${target}: retry ${attempt}/${attempts} after ${wait}s`);
        await sleep(wait);
      }
      log(`publish ${target} (attempt ${attempt}/${attempts})${dryRun ? ' [dry run]' : ''}`);
      if (dryRun) { live.add(target); break; }
      const status = await publish(target, packages.get(target));
      if (status === 0) { accepted.add(target); log(`${target}: accepted by the Marketplace`); break; }
      warn(`${target}: vsce publish exited with status ${status}`);
    }
  }

  // Exit 0 from `vsce publish` means accepted, not listed: the gallery indexes
  // asynchronously. Poll for every target still missing, including those whose
  // publish failed (a timed-out upload can be accepted server-side and appear
  // later). The first poll is immediate; the deadline is read from the clock so
  // slow `vsce show` calls count against it; the last sleep is clamped so the
  // final poll lands on the deadline instead of overshooting it.
  let polls = 0;
  if (!dryRun && missingTargets(live, wanted).length > 0) {
    const start = now();
    const deadline = start + deadlineSeconds * 1000;
    for (;;) {
      polls += 1;
      const read = await refresh(`listing poll ${polls}`);
      const missing = missingTargets(live, wanted);
      log(`listing poll ${polls} (+${Math.round((now() - start) / 1000)}s)${read ? '' : ' [read failed, last known state]'}: listed: ${sorted(live)}; missing: ${missing.join(', ') || '(none)'}`);
      if (missing.length === 0) break;
      const remaining = deadline - now();
      if (remaining <= 0) break;
      await sleep(Math.min(pollSeconds, remaining / 1000));
    }
  }

  const missing = missingTargets(live, wanted);
  return {
    version,
    dryRun,
    wanted,
    live,
    polls,
    deadlineSeconds,
    accepted: wanted.filter(target => accepted.has(target)),
    acceptedMissing: missing.filter(target => accepted.has(target)),
    failedMissing: missing.filter(target => !accepted.has(target)),
  };
}

function formatDuration(seconds) {
  if (seconds % 60 !== 0) return `${seconds} seconds`;
  const minutes = seconds / 60;
  return `${minutes} minute${minutes === 1 ? '' : 's'}`;
}

/** The error lines for a convergePublication result; empty when every target is listed. */
export function describeOutcome(result) {
  const lines = [];
  if (result.acceptedMissing.length > 0) {
    lines.push(`${EXTENSION} ${result.version}: accepted by the Marketplace but not yet listed after ${formatDuration(result.deadlineSeconds)}: ${result.acceptedMissing.join(', ')}. This is most likely gallery indexing delay. Confirm with \`vsce show ${EXTENSION} --json\` before rerunning; a rerun converges once they are listed.`);
  }
  if (result.failedMissing.length > 0) {
    lines.push(`${EXTENSION} ${result.version}: publish failed and not listed: ${result.failedMissing.join(', ')}. Check the vsce error above. If it says the version already exists, an earlier attempt was accepted and this is the same indexing delay; otherwise follow RELEASING.md's channel-specific recovery for exactly these targets.`);
  }
  return lines;
}

async function selfTest() {
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

  // File discovery and validation, separate from convergence.
  const rejects = (fileNames, pattern, what) => {
    try { validatePackages('/r', '0.7.0', fileNames); } catch (error) { assert(pattern.test(error.message), `${what}: unexpected message ${JSON.stringify(error.message)}`); return; }
    throw new Error(`self-test failed: ${what} was accepted`);
  };
  const found = validatePackages('/r', '0.7.0', ['cvc-darwin-arm64-0.7.0.vsix', 'cvc-linux-x64-0.7.0.vsix']);
  assert(found.size === 2 && found.get('linux-x64') === resolve('/r', 'cvc-linux-x64-0.7.0.vsix'), 'valid files map target to resolved path');
  rejects([], /No \.vsix files in \/r/, 'empty directory');
  rejects(['cvc-linux-x64-0.6.0.vsix'], /is version 0\.6\.0, release is 0\.7\.0/, 'wrong version');
  rejects(['cvc-linux-x64-0.7.0.vsix', 'other-linux-x64-0.7.0.vsix'], /Duplicate VSIX for target linux-x64/, 'duplicate target');
  rejects(['cvc-alpine-x64-0.7.0.vsix'], /Unexpected targets: alpine-x64/, 'unexpected target');

  // Convergence against a fake world: a fake clock that only the injected
  // `sleep`, `fetchLive` and `publish` advance, so nothing waits or touches the
  // network. The fake Marketplace tracks which targets it has accepted (publish
  // exit 0, or `silentlyAccepted`: accepted server-side although the client saw
  // a failure, like a gallery timeout). `listing` decides what it shows of that:
  // it gets the accepted targets, the 1-based read number (read 1 is the
  // pre-publish read) and the reads since the last publish call, and returns the
  // listed targets or throws.
  const ALL = [...EXPECTED_TARGETS];
  const packages = new Map(ALL.map(target => [target, `/r/cvc-${target}-0.7.0.vsix`]));
  const countOf = (items, item) => items.filter(candidate => candidate === item).length;
  function world({ listing, publishStatus = () => 0, silentlyAccepted = [], fetchSeconds = 0 }) {
    let clock = 0;
    const accepted = new Set();
    const w = { sleeps: [], publishCalls: [], reads: 0, readsSincePublish: 0, logs: [], warns: [], elapsed: () => clock / 1000 };
    w.deps = {
      now: () => clock,
      sleep: async seconds => { w.sleeps.push(seconds); clock += seconds * 1000; },
      fetchLive: () => {
        w.reads += 1; w.readsSincePublish += 1; clock += fetchSeconds * 1000;
        return new Set(listing({ accepted: [...accepted], read: w.reads, sincePublish: w.readsSincePublish }));
      },
      publish: target => {
        w.publishCalls.push(target); w.readsSincePublish = 0;
        const status = publishStatus(target, countOf(w.publishCalls, target));
        if (status === 0 || silentlyAccepted.includes(target)) accepted.add(target);
        return status;
      },
      log: message => w.logs.push(message),
      warn: message => w.warns.push(message),
    };
    w.run = (config = {}) => convergePublication({ version: '0.7.0', packages, ...config }, w.deps);
    return w;
  }
  const fullPolls = 1 + LISTING_DEADLINE_SECONDS / LISTING_POLL_SECONDS;
  const failing = failTarget => target => (target === failTarget ? 1 : 0);

  // a. Accepted, listed on the 3rd post-publish poll: success after exactly 60 s.
  let w = world({ listing: ({ accepted, sincePublish }) => (sincePublish >= 3 ? accepted : []) });
  let result = await w.run();
  assert(describeOutcome(result).length === 0 && result.polls === 3, 'a: succeeds on the 3rd poll');
  assert(w.publishCalls.length === 4 && w.reads === 4, 'a: four publishes, then exactly three polls');
  assert(w.sleeps.length === 2 && w.sleeps.every(seconds => seconds === LISTING_POLL_SECONDS) && w.elapsed() === 60, 'a: waited 60 s in two sleeps');
  assert(result.accepted.length === 4 && result.acceptedMissing.length === 0 && result.failedMissing.length === 0, 'a: everything accepted and listed');

  // b. Accepted but never listed: fail only after the deadline, as indexing delay.
  w = world({ listing: () => [] });
  result = await w.run();
  let lines = describeOutcome(result);
  assert(result.polls === fullPolls && w.reads === fullPolls + 1, `b: polls every ${LISTING_POLL_SECONDS} s up to the deadline`);
  assert(w.elapsed() >= LISTING_DEADLINE_SECONDS && w.elapsed() < LISTING_DEADLINE_SECONDS + LISTING_POLL_SECONDS, 'b: failed only once the deadline passed');
  assert(result.acceptedMissing.join() === ALL.join() && result.failedMissing.length === 0, 'b: all targets are accepted-not-listed, none failed');
  assert(lines.length === 1 && lines[0].includes('accepted by the Marketplace but not yet listed after 10 minutes') && lines[0].includes('gallery indexing delay'), 'b: message names the indexing delay');
  assert(ALL.every(target => lines[0].includes(target)) && !lines[0].includes('publish failed and not listed'), 'b: message lists every target and is not the failure message');

  // c. One target's publish fails on every attempt and it never appears.
  w = world({ listing: ({ accepted }) => accepted, publishStatus: failing('darwin-x64') });
  result = await w.run();
  lines = describeOutcome(result);
  assert(countOf(w.publishCalls, 'darwin-x64') === 3 && countOf(w.publishCalls, 'linux-x64') === 1, 'c: only the failing target is retried');
  assert(w.sleeps[0] === 30 && w.sleeps[1] === 60, 'c: retry backoff goes through the injected sleep');
  assert(result.polls === fullPolls, 'c: the failed target is still polled until the deadline');
  assert(result.failedMissing.join() === 'darwin-x64' && result.acceptedMissing.length === 0, 'c: only the failed target is missing, nothing is accepted-not-listed');
  assert(lines.length === 1 && lines[0].includes('publish failed and not listed: darwin-x64') && lines[0].includes('already exists') && lines[0].includes('RELEASING.md'), 'c: message names the failed publish');
  assert(!lines[0].includes('accepted by the Marketplace but not yet listed'), 'c: message is not the indexing-delay message');
  assert(w.warns.some(message => message.includes('darwin-x64: vsce publish exited with status 1')), 'c: each failed attempt is reported');

  // d. A publish that timed out client-side but was accepted server-side shows
  // up on a later poll.
  w = world({ listing: ({ accepted, sincePublish }) => accepted.filter(target => target !== 'darwin-x64' || sincePublish >= 2), publishStatus: failing('darwin-x64'), silentlyAccepted: ['darwin-x64'] });
  result = await w.run();
  assert(describeOutcome(result).length === 0 && result.polls === 2, 'd: a failed publish that appears later still converges');
  assert(!result.accepted.includes('darwin-x64') && result.accepted.length === 3 && result.failedMissing.length === 0, 'd: the late target was never counted as accepted by this run');

  // d2. A failed publish found live by the pre-retry check is not published again.
  w = world({ listing: ({ accepted, read }) => (read > 1 ? accepted : []), publishStatus: () => 1, silentlyAccepted: ALL });
  result = await w.run();
  assert(describeOutcome(result).length === 0 && result.polls === 0, 'd2: converged before any poll');
  assert(w.publishCalls.join() === ALL.join() && w.logs.filter(message => message.includes('became live after attempt 1')).length === 4, 'd2: each target is published once and then found live');

  // e. Everything already live: no publish, no sleep, no extra read.
  w = world({ listing: () => ALL });
  result = await w.run();
  assert(describeOutcome(result).length === 0 && w.publishCalls.length === 0 && w.sleeps.length === 0 && w.reads === 1 && result.polls === 0, 'e: nothing to do when all targets are live');

  // f. A read failure during the wait is logged and survived.
  w = world({ listing: ({ accepted, read }) => { if (read === 2) throw new Error('vsce show exploded'); return accepted; } });
  result = await w.run();
  assert(describeOutcome(result).length === 0 && result.polls === 2 && w.sleeps.length === 1, 'f: converges after one failed poll');
  assert(w.warns.some(message => message.includes('vsce show exploded')) && w.logs.some(message => message.includes('read failed')), 'f: the failed poll is reported');

  // f2. Reads that fail until the deadline end as a reported result, not a crash.
  w = world({ listing: ({ read }) => { if (read > 1) throw new Error('gallery down'); return []; } });
  result = await w.run();
  assert(result.polls === fullPolls && result.acceptedMissing.length === 4 && describeOutcome(result).length === 1, 'f2: persistent read failures end at the deadline with a result');

  // f3. A failed pre-retry check does not stop the retry.
  w = world({ listing: ({ accepted, read }) => { if (read === 2) throw new Error('blip'); return accepted; }, publishStatus: (target, attempt) => (target === 'linux-x64' && attempt === 1 ? 1 : 0) });
  result = await w.run();
  assert(describeOutcome(result).length === 0 && countOf(w.publishCalls, 'linux-x64') === 2, 'f3: retries even when the pre-retry check fails');
  assert(w.warns.some(message => message.includes('blip') && message.includes('before retry 2')), 'f3: the failed check is reported');

  // g. Dry run: no publish, no waiting, no polling after the publish loop.
  w = world({ listing: () => [] });
  result = await w.run({ dryRun: true });
  assert(describeOutcome(result).length === 0 && w.publishCalls.length === 0 && w.sleeps.length === 0 && w.reads === 1 && result.polls === 0, 'g: dry run does not poll');
  assert(w.logs.filter(message => message.includes('[dry run]')).length === 4, 'g: dry run reports each target');

  // h. The deadline is measured on the clock, so slow reads count toward it.
  w = world({ listing: () => [], fetchSeconds: 20 });
  result = await w.run();
  assert(w.elapsed() >= 20 + LISTING_DEADLINE_SECONDS && result.polls < fullPolls && result.polls === 13, 'h: slow reads shorten the poll count, not the deadline');

  // i. Both groups at once are reported separately.
  w = world({ listing: ({ accepted }) => accepted.filter(target => target !== 'win32-x64'), publishStatus: failing('linux-x64') });
  result = await w.run();
  lines = describeOutcome(result);
  assert(result.failedMissing.join() === 'linux-x64' && result.acceptedMissing.join() === 'win32-x64', 'i: targets are classified by their publish outcome');
  assert(lines.length === 2 && lines[0].includes('accepted by the Marketplace but not yet listed') && lines[0].includes('win32-x64') && !lines[0].includes('linux-x64'), 'i: accepted group is reported on its own');
  assert(lines[1].includes('publish failed and not listed: linux-x64') && !lines[1].includes('win32-x64'), 'i: failed group is reported on its own');

  // j. Overridable timing: a 90 s deadline is worded in seconds.
  w = world({ listing: () => [] });
  result = await w.run({ pollSeconds: 20, deadlineSeconds: 90 });
  assert(result.polls === 6 && w.elapsed() === 90 && w.sleeps.at(-1) === 10, 'j: the last sleep is clamped to the deadline');
  assert(describeOutcome(result)[0].includes('not yet listed after 90 seconds'), 'j: the wait is worded in seconds when not whole minutes');
  console.log('publish-vsix-convergent self-tests passed.');
}

async function main(argv) {
  const options = parseArguments(argv);
  if (options.selfTest) { await selfTest(); return 0; }
  const packages = discoverPackages(options.dir, options.version);
  const result = await convergePublication({ version: options.version, packages, dryRun: options.dryRun, attempts: options.attempts });
  const problems = describeOutcome(result);
  for (const problem of problems) console.error(problem);
  if (problems.length > 0) return 1;
  console.log(`All targets live for ${options.version}: ${result.wanted.join(', ')}`);
  return 0;
}

process.exitCode = await main(process.argv.slice(2));
