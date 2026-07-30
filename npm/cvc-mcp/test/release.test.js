'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { _extractMemberForTest, checksumForAsset, getPlatformDetails, redirectAllowed, releaseConfiguration, releaseUrls } = require('../bin/release');
const packageVersion = require('../package.json').version;

test('maps only published release platforms', () => {
  assert.deepEqual(getPlatformDetails('linux', 'x64'), { platform: 'linux', osTag: 'unknown-linux-gnu', archTag: 'x86_64' });
  assert.equal(getPlatformDetails('darwin', 'arm64').archTag, 'aarch64');
  assert.equal(getPlatformDetails('win32', 'x64').osTag, 'pc-windows-msvc');
  assert.throws(() => getPlatformDetails('linux', 'arm64'), /not published/);
});

test('builds public GitHub release URLs and permits secure overrides', () => {
  const details = getPlatformDetails('linux', 'x64');
  assert.equal(releaseUrls(details, {}).archiveUrl, `https://github.com/meirka8/volute/releases/download/v${packageVersion}/cvc-x86_64-unknown-linux-gnu.tar.gz`);
  assert.equal(releaseUrls(details, { CVC_RELEASE_REPOSITORY: 'example/cvc', CVC_RELEASE_BASE_URL: 'https://github.example/' }).checksumUrl, `https://github.example/example/cvc/releases/download/v${packageVersion}/SHA256SUMS.txt`);
  assert.throws(() => releaseConfiguration({ CVC_RELEASE_BASE_URL: 'http://bad.example' }), /HTTPS/);
  assert.throws(() => releaseConfiguration({ CVC_RELEASE_BASE_URL: 'https://user:secret@example.test' }), /credentials/);
  assert.throws(() => releaseConfiguration({ CVC_RELEASE_REPOSITORY: 'owner/repo?x=1' }), /safe/);
});

test('restricts redirects to the origin or GitHub release asset hosts', () => {
  const github = new URL('https://github.com/meirka8/volute/releases/download/v1.0.0/asset');
  assert.equal(redirectAllowed(github, new URL('https://release-assets.githubusercontent.com/file')), true);
  assert.equal(redirectAllowed(github, new URL('https://github.com/other')), true);
  assert.equal(redirectAllowed(github, new URL('https://githubusercontent.com.evil.test/file')), false);
  assert.equal(redirectAllowed(github, new URL('http://github.com/file')), false);
  const enterprise = new URL('https://github.example/releases/asset');
  assert.equal(redirectAllowed(enterprise, new URL('https://cdn.example/asset')), false);
});

test('requires exactly one matching checksum entry', () => {
  const asset = 'cvc-x86_64-unknown-linux-gnu.tar.gz';
  const hash = 'a'.repeat(64);
  assert.equal(checksumForAsset(`${hash}  ${asset}\n`, asset), hash);
  assert.throws(() => checksumForAsset('', asset), /one valid checksum/);
  assert.throws(() => checksumForAsset(`${hash}  ${asset}\n${hash}  ${asset}\n`, asset), /one valid checksum/);
});

test('Unix extraction materializes only the requested regular member', { skip: process.platform === 'win32' }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-release-test-'));
  try {
    const source = path.join(root, 'source');
    fs.mkdirSync(source);
    fs.writeFileSync(path.join(source, 'cvc'), 'expected');
    fs.writeFileSync(path.join(source, 'unrelated'), 'must not extract');
    const archive = path.join(root, 'release.tar.gz');
    assert.equal(spawnSync('/usr/bin/tar', ['-czf', archive, '-C', source, 'cvc', 'unrelated']).status, 0);
    const stage = path.join(root, 'stage');
    fs.mkdirSync(stage);
    _extractMemberForTest(archive, path.join(stage, 'cvc'), 'cvc', process.platform);
    assert.equal(fs.readFileSync(path.join(stage, 'cvc'), 'utf8'), 'expected');
    assert.equal(fs.existsSync(path.join(stage, 'unrelated')), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('Unix extraction rejects a symlink member', { skip: process.platform === 'win32' }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-release-test-'));
  try {
    const source = path.join(root, 'source');
    fs.mkdirSync(source);
    fs.writeFileSync(path.join(source, 'payload'), 'payload');
    fs.symlinkSync('payload', path.join(source, 'cvc'));
    const archive = path.join(root, 'release.tar.gz');
    assert.equal(spawnSync('/usr/bin/tar', ['-czf', archive, '-C', source, 'cvc', 'payload']).status, 0);
    assert.throws(() => _extractMemberForTest(archive, path.join(root, 'output'), 'cvc', process.platform), /unsafe binary/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
