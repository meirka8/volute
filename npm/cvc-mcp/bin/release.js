'use strict';

const crypto = require('crypto');
const fs = require('fs');
const fsp = fs.promises;
const https = require('https');
const os = require('os');
const path = require('path');
const { spawn, spawnSync } = require('child_process');
const { performance } = require('perf_hooks');
const { Transform } = require('stream');
const { pipeline } = require('stream/promises');

const DEFAULT_REPOSITORY = 'meirka8/volute';
const DEFAULT_BASE_URL = 'https://github.com';
const PACKAGE_VERSION = require('../package.json').version;
const MAX_REDIRECTS = 5;
const MAX_CHECKSUM_BYTES = 1024 * 1024;
const MAX_ARCHIVE_BYTES = 256 * 1024 * 1024;
const MAX_BINARY_BYTES = 128 * 1024 * 1024;
const BINARIES = ['cvc', 'cvc-mcp', 'cvc-lsp'];
// Publication order into the stable directory. `cvc` goes last so a concurrent
// version probe never sees the new `cvc` before its sibling binaries.
const PUBLISH_ORDER = ['cvc-lsp', 'cvc-mcp', 'cvc'];
// Same name the installers (install.sh / install.ps1) use, so launcher and
// installers exclude each other.
const INSTALL_LOCK_NAME = '.cvc-install.lock';
const STALE_LOCK_MS = 10 * 60 * 1000;
const LOCK_OWNER_TOOL = 'cvc-npm-launcher';
const MAX_LOCK_OWNER_BYTES = 4096;
const VERSION_PROBE_TIMEOUT_MS = 5000;
const MAX_PROBE_OUTPUT_BYTES = 64 * 1024;
const LOCK_WAIT_MS = 5000;
const LOCK_POLL_MS = 100;
const MAX_ACQUIRE_ATTEMPTS = 5;
const LOCK_GRACE_AGE_MS = 1000; // an empty lock younger than this may still be getting its record
const LOCK_GRACE_WAIT_MS = 500;
const BOOT_MARGIN_MS = 60 * 1000;
const LIVE_LOCK_MAX_AGE_MS = 60 * 1000; // a real publish takes seconds; an older record with a live-looking pid is pid reuse
const ORPHAN_STAGING_AGE_MS = 10 * 60 * 1000;

function validVersion(version) {
  return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(version);
}

function binaryFileName(name, platform) {
  return platform === 'win32' ? `${name}.exe` : name;
}

function compareNumeric(a, b) {
  const x = a.replace(/^0+(?=\d)/, '');
  const y = b.replace(/^0+(?=\d)/, '');
  if (x.length !== y.length) return x.length < y.length ? -1 : 1;
  return x === y ? 0 : (x < y ? -1 : 1);
}

function parseSemver(version) {
  if (typeof version !== 'string' || !validVersion(version)) throw new TypeError(`Invalid version: ${version}`);
  const dash = version.indexOf('-');
  return {
    core: (dash === -1 ? version : version.slice(0, dash)).split('.'),
    prerelease: dash === -1 ? [] : version.slice(dash + 1).split('.'),
  };
}

// SemVer 2.0.0 precedence: -1 when a < b, 0 when equal, 1 when a > b.
function compareSemver(a, b) {
  const left = parseSemver(a);
  const right = parseSemver(b);
  for (let i = 0; i < 3; i++) {
    const order = compareNumeric(left.core[i], right.core[i]);
    if (order !== 0) return order;
  }
  if (left.prerelease.length === 0 || right.prerelease.length === 0) {
    if (left.prerelease.length === right.prerelease.length) return 0;
    return left.prerelease.length === 0 ? 1 : -1;
  }
  const shared = Math.min(left.prerelease.length, right.prerelease.length);
  for (let i = 0; i < shared; i++) {
    const p = left.prerelease[i];
    const q = right.prerelease[i];
    const pNumeric = /^\d+$/.test(p);
    const qNumeric = /^\d+$/.test(q);
    if (pNumeric && qNumeric) {
      const order = compareNumeric(p, q);
      if (order !== 0) return order;
    } else if (pNumeric !== qNumeric) {
      return pNumeric ? -1 : 1;
    } else if (p !== q) {
      return p < q ? -1 : 1;
    }
  }
  if (left.prerelease.length === right.prerelease.length) return 0;
  return left.prerelease.length < right.prerelease.length ? -1 : 1;
}

function releaseConfiguration(env = process.env) {
  const repository = env.CVC_RELEASE_REPOSITORY || DEFAULT_REPOSITORY;
  const baseUrl = (env.CVC_RELEASE_BASE_URL || DEFAULT_BASE_URL).replace(/\/+$/, '');
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository) || repository.includes('..')) {
    throw new Error('CVC_RELEASE_REPOSITORY must be a safe owner/repository name.');
  }
  const parsed = new URL(baseUrl);
  if (parsed.protocol !== 'https:') throw new Error('CVC_RELEASE_BASE_URL must use HTTPS.');
  if (parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new Error('CVC_RELEASE_BASE_URL must not contain credentials, a query, or a fragment.');
  }
  if (!validVersion(PACKAGE_VERSION)) throw new Error('The npm package has an invalid release version.');
  return { repository, baseUrl, version: PACKAGE_VERSION };
}

function getPlatformDetails(platform = os.platform(), arch = os.arch()) {
  if (platform === 'linux' && arch === 'x64') return { platform, osTag: 'unknown-linux-gnu', archTag: 'x86_64' };
  if (platform === 'darwin' && arch === 'x64') return { platform, osTag: 'apple-darwin', archTag: 'x86_64' };
  if (platform === 'darwin' && arch === 'arm64') return { platform, osTag: 'apple-darwin', archTag: 'aarch64' };
  if (platform === 'win32' && arch === 'x64') return { platform, osTag: 'pc-windows-msvc', archTag: 'x86_64' };
  if (platform === 'linux' && arch === 'arm64') throw new Error('Unsupported architecture: Linux arm64 releases are not published.');
  throw new Error(`Unsupported platform or architecture: ${platform}/${arch}`);
}

function releaseUrls(details, env = process.env) {
  const { repository, baseUrl, version } = releaseConfiguration(env);
  const ext = details.platform === 'win32' ? 'zip' : 'tar.gz';
  const assetName = `cvc-${details.archTag}-${details.osTag}.${ext}`;
  const releaseUrl = `${baseUrl}/${repository}/releases/download/v${version}`;
  return { assetName, archiveUrl: `${releaseUrl}/${assetName}`, checksumUrl: `${releaseUrl}/SHA256SUMS.txt` };
}

function checksumForAsset(manifest, assetName) {
  const escaped = assetName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = new RegExp(`^([a-fA-F0-9]{64})\\s+\\*?${escaped}\\s*$`);
  const values = manifest.split(/\r?\n/).map(line => line.match(match)).filter(Boolean).map(result => result[1].toLowerCase());
  if (values.length !== 1) throw new Error(`SHA256SUMS does not contain one valid checksum for ${assetName}.`);
  return values[0];
}

function redirectAllowed(initial, target) {
  if (target.protocol !== 'https:' || target.username || target.password) return false;
  if (target.host === initial.host) return true;
  return initial.hostname === 'github.com' &&
    (target.hostname === 'githubusercontent.com' || target.hostname.endsWith('.githubusercontent.com')) &&
    target.port === '';
}

function request(url, redirects = 0, initialUrl) {
  return new Promise((resolve, reject) => {
    let parsed;
    try { parsed = new URL(url); } catch (error) { reject(error); return; }
    const initial = initialUrl || parsed;
    if (parsed.protocol !== 'https:' || parsed.username || parsed.password) {
      reject(new Error(`Refusing unsafe download URL: ${parsed.href}`)); return;
    }
    const req = https.get(parsed, { headers: { 'User-Agent': 'cvc-mcp-npm-launcher' } }, response => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        response.resume();
        if (redirects >= MAX_REDIRECTS || !response.headers.location) {
          reject(new Error(`Too many or invalid redirects while downloading ${initial.href}`)); return;
        }
        const target = new URL(response.headers.location, parsed);
        if (!redirectAllowed(initial, target)) {
          reject(new Error(`Refusing cross-origin or insecure redirect to ${target.origin}`)); return;
        }
        request(target.href, redirects + 1, initial).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume(); reject(new Error(`Download failed (${response.statusCode}) for ${initial.href}`)); return;
      }
      resolve(response);
    });
    req.setTimeout(30000, () => req.destroy(new Error(`Download timed out for ${initial.href}`)));
    req.on('error', reject);
  });
}

async function downloadFile(url, destination, maximumBytes = MAX_ARCHIVE_BYTES) {
  const response = await request(url);
  const advertised = Number(response.headers['content-length']);
  if (Number.isFinite(advertised) && advertised > maximumBytes) {
    response.destroy(); throw new Error('Release download is unexpectedly large.');
  }
  let length = 0;
  const limiter = new Transform({
    transform(chunk, _encoding, callback) {
      length += chunk.length;
      callback(length > maximumBytes ? new Error('Release download is unexpectedly large.') : null, chunk);
    },
  });
  const file = fs.createWriteStream(destination, { flags: 'wx', mode: 0o600 });
  await pipeline(response, limiter, file);
}

async function downloadText(url) {
  const response = await request(url);
  const chunks = [];
  let length = 0;
  return new Promise((resolve, reject) => {
    response.on('data', chunk => {
      length += chunk.length;
      if (length > MAX_CHECKSUM_BYTES) { response.destroy(new Error('SHA256SUMS is unexpectedly large.')); return; }
      chunks.push(chunk);
    });
    response.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    response.on('error', reject);
  });
}

async function sha256(file) {
  const hash = crypto.createHash('sha256');
  await pipeline(fs.createReadStream(file), new Transform({ transform(chunk, _encoding, callback) { hash.update(chunk); callback(); } }));
  return hash.digest('hex');
}

function trustedExecutable() {
  if (process.platform === 'win32') return path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  return '/usr/bin/tar';
}

function extractMember(archive, destination, member, platform) {
  const fd = fs.openSync(destination, 'wx', 0o700);
  try {
    let result;
    if (platform === 'win32') {
      const script = path.join(path.dirname(archive), 'extract-member.ps1');
      if (!fs.existsSync(script)) {
        fs.writeFileSync(script, [
          'param([string]$Archive,[string]$Member,[string]$Destination,[long]$Maximum)',
          'Add-Type -AssemblyName System.IO.Compression.FileSystem',
          '$zip = [IO.Compression.ZipFile]::OpenRead($Archive)',
          'try {',
          '  $entries = @($zip.Entries | Where-Object { $_.FullName -ceq $Member })',
          '  if ($entries.Count -ne 1 -or $entries[0].Length -le 0 -or $entries[0].Length -gt $Maximum) { throw "Unsafe or missing archive member: $Member" }',
          '  $input = $entries[0].Open(); $output = [IO.File]::Open($Destination, [IO.FileMode]::Truncate, [IO.FileAccess]::Write, [IO.FileShare]::None)',
          '  try {',
          '    $buffer = New-Object byte[] 65536; [long]$total = 0',
          '    while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) { $total += $read; if ($total -gt $Maximum) { throw "Expanded member is too large: $Member" }; $output.Write($buffer, 0, $read) }',
          '    $output.Flush($true)',
          '  } finally { $output.Dispose(); $input.Dispose() }',
          '} finally { $zip.Dispose() }',
        ].join('\n'), { mode: 0o600 });
      }
      fs.closeSync(fd);
      result = spawnSync(trustedExecutable(), ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script, archive, member, destination, String(MAX_BINARY_BYTES)], { stdio: ['ignore', 'ignore', 'inherit'] });
    } else {
      // Limit the child output at the OS level as well as validating the final
      // file, so a compressed member cannot fill the destination filesystem.
      result = spawnSync('/bin/sh', ['-c', 'ulimit -f 262144; exec /usr/bin/tar -xOzf "$1" -- "$2"', 'cvc-extract', archive, member], { stdio: ['ignore', fd, 'inherit'] });
    }
    if (result.error || result.status !== 0) throw result.error || new Error(`Failed to extract ${member} (exit ${result.status}).`);
  } finally {
    try { fs.closeSync(fd); } catch (_) { /* PowerShell path closes it before spawning. */ }
  }
  const stat = fs.lstatSync(destination);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > MAX_BINARY_BYTES) {
    throw new Error(`Release archive contains an unsafe binary: ${member}`);
  }
}

async function safeRegularFile(file) {
  const stat = await fsp.lstat(file);
  return stat.isFile() && !stat.isSymbolicLink() && stat.size > 0 && stat.size <= MAX_BINARY_BYTES;
}

async function safeDirectory(directory) {
  const stat = await fsp.lstat(directory);
  return stat.isDirectory() && !stat.isSymbolicLink();
}

async function ensurePrivateDirectory(directory) {
  await fsp.mkdir(directory, { recursive: true, mode: 0o700 });
  const stat = await fsp.lstat(directory).catch(() => null);
  // On Unix the directory must also belong to the current user: a directory
  // someone else owns could be swapped for anything and could not be chmod'ed.
  const owned = typeof process.getuid !== 'function' || (stat !== null && stat.uid === process.getuid());
  if (stat === null || !stat.isDirectory() || stat.isSymbolicLink() || !owned) {
    throw new Error(`Refusing unsafe cache directory: ${directory}`);
  }
  await fsp.chmod(directory, 0o700);
}

async function validCache(cacheDir, names) {
  if (!await safeDirectory(cacheDir).catch(() => false)) return false;
  for (const name of names) if (!await safeRegularFile(path.join(cacheDir, name)).catch(() => false)) return false;
  return true;
}

async function installRelease(details, executable) {
  const { version } = releaseConfiguration();
  const names = BINARIES.map(name => binaryFileName(name, details.platform));
  const binaryName = binaryFileName(executable, details.platform);
  const cacheRoot = path.join(os.homedir(), '.cvc', 'mcp-cache');
  const cacheDir = path.join(cacheRoot, `v${version}-${details.archTag}-${details.osTag}`);
  const binaryPath = path.join(cacheDir, binaryName);
  await ensurePrivateDirectory(path.join(os.homedir(), '.cvc'));
  await ensurePrivateDirectory(cacheRoot);
  if (await validCache(cacheDir, names)) return binaryPath;
  if (await fsp.lstat(cacheDir).catch(() => null)) {
    if (!await safeDirectory(cacheDir).catch(() => false)) throw new Error(`Refusing unsafe cache path: ${cacheDir}`);
    await fsp.rm(cacheDir, { recursive: true, force: true });
  }

  const tempDir = await fsp.mkdtemp(path.join(os.tmpdir(), 'cvc-mcp-'));
  let publishDir;
  try {
    const urls = releaseUrls(details);
    const archive = path.join(tempDir, urls.assetName);
    console.error(`Downloading CVC release from ${urls.archiveUrl}...`);
    const expected = checksumForAsset(await downloadText(urls.checksumUrl), urls.assetName);
    await downloadFile(urls.archiveUrl, archive);
    if (!crypto.timingSafeEqual(Buffer.from(await sha256(archive), 'hex'), Buffer.from(expected, 'hex'))) {
      throw new Error(`Checksum verification failed for ${urls.assetName}.`);
    }
    const stage = path.join(tempDir, 'stage');
    await fsp.mkdir(stage, { mode: 0o700 });
    console.error('Extracting verified members...');
    for (const name of names) extractMember(archive, path.join(stage, name), name, details.platform);

    publishDir = await fsp.mkdtemp(path.join(cacheRoot, '.install-'));
    for (const name of names) {
      const target = path.join(publishDir, name);
      await fsp.copyFile(path.join(stage, name), target, fs.constants.COPYFILE_EXCL);
      if (details.platform !== 'win32') await fsp.chmod(target, 0o755);
    }
    try {
      await fsp.rename(publishDir, cacheDir);
      publishDir = undefined;
    } catch (error) {
      if (error.code !== 'EEXIST' && error.code !== 'ENOTEMPTY') throw error;
      if (!await validCache(cacheDir, names)) throw new Error('A concurrent cache installation was incomplete or unsafe.');
    }
    console.error('Download and extraction complete.');
    return binaryPath;
  } finally {
    if (publishDir) await fsp.rm(publishDir, { recursive: true, force: true }).catch(() => {});
    await fsp.rm(tempDir, { recursive: true, force: true }).catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// Stable binary directory (~/.cvc/bin)
//
// installRelease() leaves verified binaries in a versioned private cache. Hook
// entries written by `cvc harness install` record the absolute path of the
// running `cvc`, so running out of the versioned cache would pin hooks to a
// stale version forever. Like install.sh / install.ps1, the launcher therefore
// publishes the binaries into ~/.cvc/bin and runs them from there. The npm
// package version is a floor, never a pin: a newer installed version is left
// alone and the launcher never downgrades.
//
// Everything here writes to stderr only: cvc-mcp speaks MCP JSON-RPC on stdout.
// ---------------------------------------------------------------------------

function publishContext(options = {}) {
  const home = options.home ?? os.homedir();
  const platform = options.platform ?? process.platform;
  const packageVersion = options.packageVersion ?? PACKAGE_VERSION;
  if (!validVersion(packageVersion)) throw new Error('the npm package has an invalid release version');
  return {
    home,
    platform,
    packageVersion,
    stableDir: path.join(home, '.cvc', 'bin'),
    names: BINARIES.map(name => binaryFileName(name, platform)),
    // Version probes keyed by file identity, so the decision re-run under the
    // lock does not pay a second timeout for an unchanged, unresponsive `cvc`.
    probed: new Map(),
    // Internal seams for tests: host identity overrides, lock-wait and probe timing,
    // and the function that starts the version probe.
    system: options.system ?? {},
    lockWaitMs: options.lockWaitMs ?? LOCK_WAIT_MS,
    lockPollMs: options.lockPollMs ?? LOCK_POLL_MS,
    probeTimeoutMs: options.probeTimeoutMs ?? VERSION_PROBE_TIMEOUT_MS,
    spawnSync: options.spawnSync ?? spawnSync,
  };
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function inspectPath(file) {
  try {
    const stat = await fsp.lstat(file);
    let kind = 'other';
    if (stat.isSymbolicLink()) kind = 'symlink';
    else if (stat.isDirectory()) kind = 'directory';
    else if (stat.isFile()) kind = 'file';
    return { kind, size: stat.size, mode: stat.mode, mtimeMs: stat.mtimeMs, identity: `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeMs}:${stat.ctimeMs}` };
  } catch (error) {
    if (error.code === 'ENOENT') return { kind: 'absent', size: 0, mode: 0, mtimeMs: 0, identity: 'absent' };
    throw error;
  }
}

// What `cvc --version` told us about an installed binary:
//   known       - it printed `cvc X.Y.Z` (optionally `+build` metadata and trailing text) to
//                 stdout and exited 0 or, for releases before v0.7.0 whose generic error path
//                 handled --version, to stderr with exit status 1 and nothing on stdout
//   cannot-run  - the file itself cannot be executed: a format the OS rejects (ENOEXEC,
//                 EFTYPE, or the shell's exit 126), or EACCES on a file with no execute
//                 bit. A broken install that is safe to repair.
//   unknown     - anything else: it timed out, failed, printed something unrecognised, or
//                 could not start for reasons that describe the ENVIRONMENT rather than
//                 the file (a missing loader, interpreter or shared library: ENOENT or
//                 exit 127; EACCES on an executable file: noexec mount, AppArmor,
//                 SELinux). The same file may well run on the host that installed it,
//                 so it is left exactly as it is.
// Every outcome except `known` carries a `reason` for the stderr line.
// Only `cvc` is ever probed: `cvc-mcp` blocks waiting on stdin.
const VERSION_LINE = /^cvc (\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\+[0-9A-Za-z.-]+)?(?:\s.*)?$/;

// First line of a probe stream, if it has the shape of the version line.
function versionFromFirstLine(stream) {
  const match = typeof stream === 'string' ? VERSION_LINE.exec(stream.split(/\r?\n/, 1)[0]) : null;
  return match && validVersion(match[1]) ? match[1] : null;
}

function probeInstalledVersion(cvcPath, { timeoutMs = VERSION_PROBE_TIMEOUT_MS, mode, platform = process.platform, spawn: spawnProbe = spawnSync } = {}) {
  const result = spawnProbe(cvcPath, ['--version'], {
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: timeoutMs,
    killSignal: 'SIGKILL', // a child that traps SIGTERM must not outlive the timeout
    maxBuffer: MAX_PROBE_OUTPUT_BYTES,
    windowsHide: true,
    encoding: 'utf8',
  });
  if (result.error) {
    const code = result.error.code;
    if (code === 'ETIMEDOUT') return { kind: 'unknown', reason: 'timed out' };
    if (code === 'ENOBUFS') return { kind: 'unknown', reason: 'unrecognised output' };
    const reason = `could not be started: ${code || result.error.message}`;
    if (code === 'ENOEXEC' || code === 'EFTYPE') return { kind: 'cannot-run', reason };
    // EACCES describes the file only when it has no execute permission at all. On
    // Windows there are no execute bits to consult, so it is never conclusive there.
    if (code === 'EACCES' && platform !== 'win32' && mode !== undefined && (mode & 0o111) === 0) return { kind: 'cannot-run', reason };
    return { kind: 'unknown', reason };
  }
  if (result.status === null) return { kind: 'unknown', reason: `terminated by ${result.signal || 'a signal'}` };
  // 126 is the shell's "found but cannot execute" (a malformed binary retried through
  // /bin/sh by the C library). 127, "not found", also covers a missing shared library
  // and is therefore not conclusive about the file.
  if (result.status === 126) return { kind: 'cannot-run', reason: 'exit 126' };
  if (result.status === 1 && result.stdout === '') {
    // Releases before v0.7.0 print the version through the generic error path: to
    // stderr, exiting 1. Only that exact shape is accepted; any other failure is unknown.
    const legacy = versionFromFirstLine(result.stderr);
    if (legacy !== null) return { kind: 'known', version: legacy };
  }
  if (result.status !== 0) return { kind: 'unknown', reason: `exit ${result.status}` };
  const version = versionFromFirstLine(result.stdout);
  if (version !== null) return { kind: 'known', version };
  return { kind: 'unknown', reason: 'unrecognised output' };
}

function probeOptions(ctx, mode) {
  return { timeoutMs: ctx.probeTimeoutMs, mode, platform: ctx.platform, spawn: ctx.spawnSync };
}

// Throws unless `outcome` is a successful probe of exactly the package version.
function requireProbedVersion(ctx, label, cvcName, outcome) {
  if (outcome.kind === 'known' && compareSemver(outcome.version, ctx.packageVersion) === 0) return;
  const reports = outcome.kind === 'known' ? outcome.version : `no usable version (${outcome.reason})`;
  throw new Error(`the ${label} ${cvcName} reports ${reports} instead of ${ctx.packageVersion}`);
}

// Decides whether the stable directory must be (re)published. `target` is the
// stable binary to run when nothing needs publishing, or null when the stable
// copy of the requested binary is unusable and the cache is needed instead.
// `blocked` means an installed `cvc` exists but would not say what version it
// is, so it must be left exactly as it is. Throws when the stable directory
// itself is unsafe to use.
async function decidePublication(ctx, binaryName) {
  const directory = await inspectPath(ctx.stableDir);
  if (directory.kind === 'absent') return { publish: true };
  if (directory.kind !== 'directory') {
    throw new Error(`${ctx.stableDir} is not a real directory (symlinks and files are refused)`);
  }
  const usable = {};
  const entries = {};
  for (const name of ctx.names) {
    entries[name] = await inspectPath(path.join(ctx.stableDir, name));
    usable[name] = entries[name].kind === 'file' && entries[name].size > 0;
  }
  const cvcName = binaryFileName('cvc', ctx.platform);
  const cvcPath = path.join(ctx.stableDir, cvcName);
  let installed = null;
  if (usable[cvcName]) {
    const identity = entries[cvcName].identity;
    if (!ctx.probed.has(identity)) ctx.probed.set(identity, probeInstalledVersion(cvcPath, probeOptions(ctx, entries[cvcName].mode)));
    const outcome = ctx.probed.get(identity);
    if (outcome.kind === 'unknown') {
      return { publish: false, target: null, blocked: `${cvcPath} did not report a usable version (${outcome.reason}), so it was left in place; delete it to have the launcher replace it` };
    }
    if (outcome.kind === 'known') installed = outcome.version;
  }
  if (installed !== null) {
    const order = compareSemver(installed, ctx.packageVersion);
    const stableBinary = path.join(ctx.stableDir, binaryName);
    // Newer than this package: never touch it, never downgrade.
    if (order > 0) return { publish: false, target: usable[binaryName] ? stableBinary : null };
    // Same version and complete: leave it alone (it may be a local build).
    if (order === 0 && ctx.names.every(name => usable[name])) return { publish: false, target: stableBinary };
  }
  return { publish: true };
}

async function ensureStableDirectory(ctx) {
  await fsp.mkdir(ctx.stableDir, { recursive: true, mode: 0o700 });
  if ((await inspectPath(ctx.stableDir)).kind !== 'directory') {
    throw new Error(`${ctx.stableDir} is not a real directory (symlinks and files are refused)`);
  }
}

// The lock is a directory on every platform: install.sh creates a directory and
// install.ps1 creates a file at the same path, and an existing entry of either
// kind makes the other's exclusive create fail, so launcher and installers
// still exclude each other. Inside it sits one `owner-<nonce>` file recording
// who holds it. The record lets a later launch recover from a launcher that was
// killed mid-publish (Node exits on SIGTERM without running `finally`), but only
// a lock this launcher created is ever recoverable: locks from the installers
// carry no record and are never broken.
//
// The nonce is in the file NAME so a lock can be broken with a compare-and-swap:
// a breaker claims the dead lock by renaming that exact `owner-<nonce>` file.
// That rename succeeds for exactly one racer and fails with ENOENT if the lock
// was released, broken, or replaced by a live one in the meantime, so a live
// lock can never be displaced. (Moving the whole lock directory aside cannot
// give this guarantee: a competitor's fresh lock could be moved and then could
// not be restored atomically.)
const LOCK_OWNER_ENTRY = /^owner-[0-9a-f]{16}$/;

class LockHeldError extends Error {
  constructor(message, lockPath, live) {
    super(message);
    this.lockPath = lockPath;
    this.live = live; // held by a launcher that is publishing right now
  }
}

function readTrimmed(file) {
  try { return fs.readFileSync(file, 'utf8').trim() || null; } catch (_) { return null; }
}

// What identifies this host, boot and PID namespace; any field can be overridden
// through ctx.system by tests. Fields that cannot be determined are null.
function currentSystem(ctx) {
  let pidns = null;
  if (process.platform === 'linux') {
    try { pidns = fs.readlinkSync('/proc/self/ns/pid') || null; } catch (_) { /* unknown */ }
  }
  return {
    host: os.hostname(),
    boot: process.platform === 'linux' ? readTrimmed('/proc/sys/kernel/random/boot_id') : null,
    pidns,
    uptimeMs: os.uptime() * 1000,
    now: Date.now(),
    ...ctx.system,
  };
}

function newLockOwner(system) {
  const nonce = crypto.randomBytes(8).toString('hex');
  const raw = JSON.stringify({
    tool: LOCK_OWNER_TOOL,
    pid: process.pid,
    host: system.host,
    created: new Date().toISOString(),
    pidns: system.pidns ?? null,
    boot: system.boot ?? null,
    nonce,
  });
  return { raw, entry: `owner-${nonce}` };
}

// Bounded, symlink-refusing read of a lock's single owner record. Returns
// { file, raw, record } or null for anything else: a missing or empty lock (an
// installer's), extra entries, an oversized or malformed record.
async function readLockOwner(lockPath) {
  try {
    if ((await inspectPath(lockPath)).kind !== 'directory') return null;
    const entries = await fsp.readdir(lockPath);
    if (entries.length !== 1 || !LOCK_OWNER_ENTRY.test(entries[0])) return null;
    const file = path.join(lockPath, entries[0]);
    const handle = await fsp.open(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.size > MAX_LOCK_OWNER_BYTES) return null;
      const buffer = Buffer.alloc(MAX_LOCK_OWNER_BYTES + 1);
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
      if (bytesRead > MAX_LOCK_OWNER_BYTES) return null;
      const raw = buffer.toString('utf8', 0, bytesRead);
      const record = JSON.parse(raw);
      return record !== null && typeof record === 'object' && !Array.isArray(record) ? { file, raw, record } : null;
    } finally {
      await handle.close();
    }
  } catch (_) {
    return null;
  }
}

const known = value => typeof value === 'string' && value !== '';

// Is the process that wrote this lock record gone?
//   'dead'    - provably gone: the lock may be broken
//   'live'    - still running here: another launcher is publishing right now
//   'foreign' - not a record we can judge (other tool, other host, a boot id or PID
//               namespace known on only one side, a different PID namespace,
//               malformed) or too old to be a publish in progress: never broken,
//               never waited for
// Rules, in order. Boot id: both known and different means the machine restarted;
// known on only one side means a different kind of environment sharing the host name
// and home (Windows and WSL, say), which cannot be judged; unknown on both sides falls
// back to whether the record predates this boot. PID namespace: known on only one side
// or different on the two sides cannot be judged by signalling a pid. Otherwise the pid
// decides: ESRCH means gone, EPERM or success means alive, and an alive pid behind a
// record older than a real publish could take is pid reuse, not a live launcher.
function lockOwnerState(record, system) {
  if (record.tool !== LOCK_OWNER_TOOL || record.host !== system.host) return 'foreign';
  if (!Number.isSafeInteger(record.pid) || record.pid <= 0) return 'foreign';
  const recordBoot = known(record.boot);
  if (recordBoot !== known(system.boot)) return 'foreign';
  if (recordBoot) {
    if (record.boot !== system.boot) return 'dead';
  } else if (typeof record.created === 'string') {
    const created = Date.parse(record.created);
    if (Number.isFinite(created) && created < system.now - system.uptimeMs - BOOT_MARGIN_MS) return 'dead';
  }
  const recordPidns = known(record.pidns);
  if (recordPidns !== known(system.pidns)) return 'foreign';
  if (recordPidns && record.pidns !== system.pidns) return 'foreign';
  try {
    process.kill(record.pid, 0);
  } catch (error) {
    if (error.code === 'ESRCH') return 'dead';
  }
  const created = typeof record.created === 'string' ? Date.parse(record.created) : NaN;
  return Number.isFinite(created) && system.now - created <= LIVE_LOCK_MAX_AGE_MS ? 'live' : 'foreign';
}

// Classifies whatever currently occupies the lock path.
async function inspectHeldLock(ctx, lockPath) {
  const system = currentSystem(ctx);
  const graceUntil = performance.now() + LOCK_GRACE_WAIT_MS; // monotonic: immune to clock changes
  for (;;) {
    const entry = await inspectPath(lockPath);
    if (entry.kind === 'absent') return { state: 'gone' };
    const owner = await readLockOwner(lockPath);
    if (owner !== null) return { state: lockOwnerState(owner.record, system), owner };
    // No readable record. A lock directory that was only just created or just
    // emptied is most likely a launcher between creating it and finishing its
    // record (still empty, or the record file created but not yet written), or
    // between removing the record and the directory. Give that a moment before
    // treating it as an installer's lock, whose directory stays empty. The
    // directory's age is read again after listing it: an unlink that emptied it
    // moments ago refreshes its mtime, which the earlier stat could not show.
    if (entry.kind === 'directory' && performance.now() < graceUntil) {
      let names = null;
      try {
        names = await fsp.readdir(lockPath);
      } catch (error) {
        if (error.code === 'ENOENT') continue; // removed under us (possibly replaced): classify afresh
      }
      if (names !== null && (names.length === 0 || (names.length === 1 && LOCK_OWNER_ENTRY.test(names[0])))) {
        const fresh = await inspectPath(lockPath);
        if (fresh.kind === 'absent' || (fresh.kind === 'directory' && Date.now() - fresh.mtimeMs < LOCK_GRACE_AGE_MS)) {
          await sleep(20);
          continue;
        }
      }
    }
    return { state: 'foreign' };
  }
}

// rmdir of the lock directory. On Windows an antivirus or indexer can briefly
// hold the just-deleted owner file, so ENOTEMPTY/EBUSY/EPERM are retried.
async function removeLockDirectory(ctx, lockPath) {
  const attempts = ctx.platform === 'win32' ? 5 : 1;
  for (let attempt = 1; ; attempt++) {
    try {
      await fsp.rmdir(lockPath);
      return;
    } catch (error) {
      if (error.code === 'ENOENT') return;
      if (attempt >= attempts || !['ENOTEMPTY', 'EBUSY', 'EPERM'].includes(error.code)) throw error;
      await sleep(50);
    }
  }
}

// Removes a lock whose launcher is provably dead. Whether we win the claim or a
// rival does (or the lock changed under us), the caller just tries again.
async function breakStaleLock(ctx, lockPath, judged) {
  // Claim: only one racer can rename the judged file, and only while it is
  // still in the lock instance that was judged. The claimed name still matches
  // the owner pattern and still holds the dead record, so if we are killed from
  // here on, the next launch simply breaks the lock again.
  const claimed = path.join(lockPath, `owner-${crypto.randomBytes(8).toString('hex')}`);
  try {
    await fsp.rename(judged.file, claimed);
  } catch (error) {
    if (error.code === 'ENOENT') return; // released, broken by another, or replaced
    throw error;
  }
  // Exactly one claimant wins this unlink (a rival may have re-claimed the file).
  try {
    await fsp.unlink(claimed);
  } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  await removeLockDirectory(ctx, lockPath);
}

function removalCommand(platform, lockPath) {
  if (platform === 'win32') return `Remove-Item -Recurse -Force '${lockPath.replace(/'/g, "''")}'`;
  return `rm -r '${lockPath.replace(/'/g, "'\\''")}'`;
}

async function lockHeldMessage(ctx, lockPath) {
  let message = `another CVC installation holds ${lockPath}`;
  const held = await inspectPath(lockPath).catch(() => null);
  if (held && held.kind !== 'absent' && Date.now() - held.mtimeMs > STALE_LOCK_MS) {
    message += ` (older than 10 minutes, so it may be stale; if no installation is running, remove it with: ${removalCommand(ctx.platform, lockPath)})`;
  }
  return message;
}

// Returns { lockPath, ownerFile, raw } only when this process created the lock;
// a lock it did not create is never removed. A dead launcher's lock is broken
// (a bounded number of attempts). Throws LockHeldError when someone else holds it
// (`live` when that is another launcher publishing right now, which is worth
// waiting for).
async function acquireInstallLock(ctx, { mayWait = true } = {}) {
  const lockPath = path.join(ctx.stableDir, INSTALL_LOCK_NAME);
  for (let attempt = 1; ; attempt++) {
    try {
      await fsp.mkdir(lockPath, { mode: 0o700 });
      break;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      const held = await inspectHeldLock(ctx, lockPath);
      // Its holder released it between our mkdir and our look: simply try again.
      if (held.state === 'gone' && attempt < MAX_ACQUIRE_ATTEMPTS) continue;
      // A dead holder's lock is broken; if a rival breaker is mid-cleanup we may
      // see its claimed (still dead) record next time round. Bounded by the attempts.
      if (held.state === 'dead' && attempt < MAX_ACQUIRE_ATTEMPTS) {
        await breakStaleLock(ctx, lockPath, held.owner);
        continue;
      }
      throw new LockHeldError(await lockHeldMessage(ctx, lockPath), lockPath, mayWait && held.state === 'live');
    }
  }
  const { raw, entry } = newLockOwner(currentSystem(ctx));
  const ownerFile = path.join(lockPath, entry);
  try {
    await fsp.writeFile(ownerFile, raw, { flag: 'wx', mode: 0o600 });
  } catch (error) {
    // We created the lock but could not label it; do not leave it behind.
    await fsp.unlink(ownerFile).catch(() => {});
    await fsp.rmdir(lockPath).catch(() => {});
    throw error;
  }
  return { lockPath, ownerFile, raw };
}

// Waits (bounded) for another launcher's lock to go away. Returns true as soon
// as the lock is gone or its holder is no longer a live launcher, false when
// the budget runs out.
async function waitForLockRelease(ctx, lockPath) {
  const deadline = performance.now() + ctx.lockWaitMs; // monotonic: immune to clock changes
  while (performance.now() < deadline) {
    await sleep(Math.min(ctx.lockPollMs, Math.max(0, deadline - performance.now())));
    if ((await inspectPath(lockPath)).kind === 'absent') return true;
    const owner = await readLockOwner(lockPath);
    if (owner !== null && lockOwnerState(owner.record, currentSystem(ctx)) !== 'live') return true;
  }
  return (await inspectPath(lockPath)).kind === 'absent';
}

// Removes the lock only if it still carries this process's own record.
async function releaseInstallLock(ctx, lock) {
  try {
    const current = await readLockOwner(lock.lockPath);
    if (current === null || current.file !== lock.ownerFile || current.raw !== lock.raw) return;
    await fsp.unlink(lock.ownerFile);
    await removeLockDirectory(ctx, lock.lockPath);
  } catch (_) { /* best effort: a lingering lock is recovered or reported by the next launch */ }
}

// An existing target must be a regular file (or absent); symlinks, directories
// and special files are refused, as the installers do.
async function replaceableTarget(target) {
  const entry = await inspectPath(target);
  if (entry.kind !== 'absent' && entry.kind !== 'file') {
    throw new Error(`refusing to replace ${target}: it is not a regular file`);
  }
  return entry.kind;
}

// Windows keeps a running .exe from being overwritten or deleted, but it can be
// renamed. Retired images are named `.cvc-old-<file>-<hex>`; delete leftovers
// from earlier publishes when they are no longer running.
async function removeRetiredImages(stableDir) {
  const retired = /^\.cvc-old-(?:cvc|cvc-mcp|cvc-lsp)(?:\.exe)?-[0-9a-f]{16}$/;
  let entries;
  try { entries = await fsp.readdir(stableDir); } catch (_) { return; }
  for (const entry of entries) {
    if (!retired.test(entry)) continue;
    try {
      const file = path.join(stableDir, entry);
      if ((await inspectPath(file)).kind === 'file') await fsp.unlink(file);
    } catch (_) { /* EBUSY/EPERM/EACCES: still running; the next publish retries. */ }
  }
}

// A publish that was killed leaves its `.cvc-install.<random>` staging
// directory behind (install.sh uses the same prefix). With the shared lock held
// no installer is mid-publish; the age gate adds margin. Only real directories
// with the exact mkdtemp shape are removed, and nothing sharing the lock's name.
async function reapOrphanedStaging(stableDir) {
  const orphan = /^\.cvc-install\.[A-Za-z0-9]{6}$/; // exactly mkdtemp's six characters
  let entries;
  try { entries = await fsp.readdir(stableDir); } catch (_) { return; }
  for (const entry of entries) {
    if (entry.startsWith(INSTALL_LOCK_NAME) || !orphan.test(entry)) continue;
    try {
      const dir = path.join(stableDir, entry);
      const stat = await inspectPath(dir);
      if (stat.kind === 'directory' && Date.now() - stat.mtimeMs > ORPHAN_STAGING_AGE_MS) {
        await fsp.rm(dir, { recursive: true, force: true });
      }
    } catch (_) { /* best effort */ }
  }
}

async function moveIntoPlace(ctx, stageDir, name) {
  const staged = path.join(stageDir, name);
  const target = path.join(ctx.stableDir, name);
  const existing = await replaceableTarget(target);
  if (ctx.platform !== 'win32') {
    await fsp.rename(staged, target); // atomic replace on the same filesystem
    return;
  }
  let retired;
  if (existing === 'file') {
    retired = path.join(ctx.stableDir, `.cvc-old-${name}-${crypto.randomBytes(8).toString('hex')}`);
    await fsp.rename(target, retired);
  }
  try {
    await fsp.rename(staged, target);
  } catch (error) {
    if (retired) await fsp.rename(retired, target).catch(() => {});
    throw error;
  }
  if (retired) await fsp.unlink(retired).catch(() => {});
}

async function publishBinaries(ctx, cacheDir) {
  if (ctx.platform === 'win32') await removeRetiredImages(ctx.stableDir);
  await reapOrphanedStaging(ctx.stableDir);
  const sizes = new Map();
  for (const name of ctx.names) {
    const source = await inspectPath(path.join(cacheDir, name));
    if (source.kind !== 'file' || source.size <= 0) throw new Error(`the cached ${name} is missing or unsafe`);
    sizes.set(name, source.size);
    await replaceableTarget(path.join(ctx.stableDir, name));
  }
  const stageDir = await fsp.mkdtemp(path.join(ctx.stableDir, '.cvc-install.'));
  try {
    for (const name of ctx.names) {
      const staged = path.join(stageDir, name);
      await fsp.copyFile(path.join(cacheDir, name), staged, fs.constants.COPYFILE_EXCL);
      if (ctx.platform !== 'win32') await fsp.chmod(staged, 0o755);
      if ((await fsp.lstat(staged)).size !== sizes.get(name)) throw new Error(`staged ${name} has an unexpected size`);
    }
    // Nothing is replaced unless the new cvc demonstrably runs here and is the right
    // version: a cache copy that cannot run in this environment must never displace an
    // installation that may work elsewhere, and would only fail the same way next launch.
    const cvcName = binaryFileName('cvc', ctx.platform);
    const stagedCvc = path.join(stageDir, cvcName);
    requireProbedVersion(ctx, 'staged', cvcName, probeInstalledVersion(stagedCvc, probeOptions(ctx, (await fsp.lstat(stagedCvc)).mode)));
    for (const base of PUBLISH_ORDER) await moveIntoPlace(ctx, stageDir, binaryFileName(base, ctx.platform));
  } finally {
    await fsp.rm(stageDir, { recursive: true, force: true }).catch(() => {});
  }
}

async function resolveLaunchTarget(ctx, cachePath, executable) {
  const binaryName = binaryFileName(executable, ctx.platform);
  let decision = await decidePublication(ctx, binaryName);
  if (decision.blocked) throw new Error(decision.blocked);
  if (!decision.publish) return decision.target ?? cachePath;

  await ensureStableDirectory(ctx);
  let lock;
  try {
    lock = await acquireInstallLock(ctx);
  } catch (error) {
    if (!(error instanceof LockHeldError) || !error.live || !await waitForLockRelease(ctx, error.lockPath)) throw error;
    // The other launcher finished (or died). It may well have done our work.
    decision = await decidePublication(ctx, binaryName);
    if (decision.blocked) throw new Error(decision.blocked);
    if (!decision.publish) return decision.target ?? cachePath;
    lock = await acquireInstallLock(ctx, { mayWait: false });
  }
  try {
    // Another launcher may have published while this one waited for the lock.
    decision = await decidePublication(ctx, binaryName);
    if (decision.blocked) throw new Error(decision.blocked);
    if (!decision.publish) return decision.target ?? cachePath;

    await publishBinaries(ctx, path.dirname(cachePath));

    const cvcName = binaryFileName('cvc', ctx.platform);
    const publishedCvc = path.join(ctx.stableDir, cvcName);
    requireProbedVersion(ctx, 'published', cvcName, probeInstalledVersion(publishedCvc, probeOptions(ctx, (await fsp.lstat(publishedCvc)).mode)));
    console.error(`Installed CVC ${ctx.packageVersion} to ${ctx.stableDir}`);
    return path.join(ctx.stableDir, binaryName);
  } finally {
    await releaseInstallLock(ctx, lock);
  }
}

// Returns the path to execute: ~/.cvc/bin/<binary> when usable, else the cache
// path. Never throws and never blocks the launch. `shared` is the context built
// by the pre-download check, so both phases reuse the same version probes.
async function ensurePublished(cachePath, executable, options = {}, shared) {
  let stableDir = '~/.cvc/bin';
  try {
    const ctx = shared ?? publishContext(options);
    stableDir = ctx.stableDir;
    return await resolveLaunchTarget(ctx, cachePath, executable);
  } catch (error) {
    console.error(`CVC: could not install into ${stableDir}: ${error.message}. Running the verified cached copy this time; the next launch tries again.`);
    return cachePath;
  }
}

function run(binary, args) {
  const child = spawn(binary, args, { stdio: 'inherit' });
  child.on('error', error => { console.error(`Failed to run ${binary}: ${error.message}`); process.exit(1); });
  child.on('close', (code, signal) => process.exit(code === null ? (signal ? 1 : 0) : code));
}

// Decides what to execute. ~/.cvc must pass the same private-directory check
// every launch has always passed through installRelease (a real directory owned
// by this user, mode 0700) before anything below it is probed or run. The
// stable directory is then checked before anything is downloaded: when it
// already holds a same-or-newer complete release there is nothing to download
// (this also keeps an old pinned launcher working offline). The release is
// fetched into the cache only when the cache is actually needed.
async function launchTarget(executable, options = {}) {
  if (!BINARIES.includes(executable)) throw new Error('Refusing unknown CVC executable.');
  await ensurePrivateDirectory(path.join(options.home ?? os.homedir(), '.cvc'));
  let ctx;
  try {
    ctx = publishContext(options);
    const decision = await decidePublication(ctx, binaryFileName(executable, ctx.platform));
    if (!decision.publish && decision.target) return decision.target;
  } catch (_) { /* ensurePublished reports it below; the launch continues with the cache */ }
  const install = options.installRelease ?? installRelease;
  const cachePath = await install(options.details ?? getPlatformDetails(), executable);
  return ensurePublished(cachePath, executable, options, ctx);
}

async function launch(executable) {
  run(await launchTarget(executable), process.argv.slice(2));
}

module.exports = {
  checksumForAsset,
  compareSemver,
  getPlatformDetails,
  launch,
  redirectAllowed,
  releaseConfiguration,
  releaseUrls,
  _extractMemberForTest: extractMember,
  _ensurePublishedForTest: ensurePublished,
  _launchTargetForTest: launchTarget,
};
