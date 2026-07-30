'use strict';

const crypto = require('crypto');
const fs = require('fs');
const fsp = fs.promises;
const https = require('https');
const os = require('os');
const path = require('path');
const { spawn, spawnSync } = require('child_process');
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

function validVersion(version) {
  return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(version);
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
      result = spawnSync(trustedExecutable(), ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script, archive, member, destination, String(MAX_BINARY_BYTES)], { stdio: 'inherit' });
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
  if (!await safeDirectory(directory).catch(() => false)) throw new Error(`Refusing unsafe cache directory: ${directory}`);
  await fsp.chmod(directory, 0o700);
}

async function validCache(cacheDir, names) {
  if (!await safeDirectory(cacheDir).catch(() => false)) return false;
  for (const name of names) if (!await safeRegularFile(path.join(cacheDir, name)).catch(() => false)) return false;
  return true;
}

async function installRelease(details, executable) {
  const { version } = releaseConfiguration();
  const names = details.platform === 'win32' ? BINARIES.map(name => `${name}.exe`) : BINARIES;
  const binaryName = details.platform === 'win32' ? `${executable}.exe` : executable;
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

function run(binary, args) {
  const child = spawn(binary, args, { stdio: 'inherit' });
  child.on('error', error => { console.error(`Failed to run ${binary}: ${error.message}`); process.exit(1); });
  child.on('close', (code, signal) => process.exit(code === null ? (signal ? 1 : 0) : code));
}

async function launch(executable) {
  if (!BINARIES.includes(executable)) throw new Error('Refusing unknown CVC executable.');
  run(await installRelease(getPlatformDetails(), executable), process.argv.slice(2));
}

module.exports = { checksumForAsset, getPlatformDetails, launch, redirectAllowed, releaseConfiguration, releaseUrls, _extractMemberForTest: extractMember };
