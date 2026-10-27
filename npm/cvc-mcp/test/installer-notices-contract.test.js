'use strict';

// Cross-component contract for issue #128, installer side.
//
// The release workflow packs LICENSE and THIRD-PARTY-NOTICES.md at the root of
// every binary archive, and both installers copy those two files into a
// share/cvc directory beside the install directory (~/.cvc/bin gives
// ~/.cvc/share/cvc). install.sh has its own offline test
// (.github/scripts/test-install-sh.sh) and install.ps1 can only be exercised on
// Windows (.github/scripts/test-install-ps1.ps1), so this reads the three
// sources and fails when the file names, the layout or the order of the steps
// drift apart.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..', '..', '..');
const release = fs.readFileSync(path.join(root, '.github', 'workflows', 'release.yml'), 'utf8');
const installSh = fs.readFileSync(path.join(root, 'install.sh'), 'utf8');
const installPs1 = fs.readFileSync(path.join(root, 'install.ps1'), 'utf8');

const NOTICES = ['LICENSE', 'THIRD-PARTY-NOTICES.md'];

function required(source, pattern, what, where) {
  const match = pattern.exec(source);
  assert.ok(match, `cannot find ${what} in ${where}; if it moved, update the matching script and this contract test`);
  return match;
}

test('release.yml packs the notices at the root of the Unix and Windows archives', () => {
  required(release, /tar -czf cvc-\$\{\{ matrix\.target \}\}\.tar\.gz [^\n]*-C "\$GITHUB_WORKSPACE" LICENSE THIRD-PARTY-NOTICES\.md\n/, 'the tar command that packs the Unix archive', 'release.yml');
  required(release, /Compress-Archive -Path [^\n]*,LICENSE,THIRD-PARTY-NOTICES\.md -DestinationPath cvc-\$\{\{ matrix\.target \}\}\.zip\n/, 'the Compress-Archive command that packs the Windows archive', 'release.yml');
});

test('install.sh stages and installs exactly the files the archives carry', () => {
  const lists = [...installSh.matchAll(/for notice in ([^;]+); do/g)].map(match => match[1].trim().split(/\s+/));
  assert.ok(lists.length >= 2, 'install.sh has no loops over the notice files');
  for (const names of lists) assert.deepEqual(names, NOTICES);
});

test('install.ps1 stages and installs exactly the files the archives carry', () => {
  const names = [...required(installPs1, /\$NoticeFiles = @\(([^)]*)\)/, '$NoticeFiles', 'install.ps1')[1].matchAll(/"([^"]+)"/g)].map(match => match[1]);
  assert.deepEqual(names, NOTICES);
});

test('both installers put the notices in share/cvc beside the install directory', () => {
  required(installSh, /SHARE_PARENT=\$\(dirname "\$INSTALL_DIR"\)\n/, 'the parent of the install directory', 'install.sh');
  required(installSh, /SHARE_DIR="\$\{SHARE_PARENT%\/\}\/share\/cvc"\n/, 'the share directory', 'install.sh');
  required(installPs1, /\$SharePrefix = \[IO\.Path\]::GetFullPath\(\(Join-Path \$InstallDir '\.\.'\)\)\n/, 'the parent of the install directory', 'install.ps1');
  required(installPs1, /\$ShareDir = Join-Path \(Join-Path \$SharePrefix 'share'\) 'cvc'\n/, 'the share directory', 'install.ps1');
});

test('install.sh installs the notices after staging every binary and before replacing the first one', () => {
  const staged = required(installSh, /cp "\$STAGE\/\$binary" "\$PUBLISH_STAGE\/\$binary"/, 'the staging of the binaries', 'install.sh');
  const notices = required(installSh, /\n\s*if publish_notices; then\n/, 'the call that installs the notices', 'install.sh');
  const moves = required(installSh, /mv -f "\$PUBLISH_STAGE\/\$binary" "\$INSTALL_DIR\/\$binary"/, 'the move loop', 'install.sh');
  assert.ok(staged.index < notices.index && notices.index < moves.index);
});

test('install.ps1 installs the notices after staging every binary and before replacing the first one', () => {
  const staged = required(installPs1, /\$Prepared\[\$Binary\] = \$Temporary/, 'the staging of the binaries', 'install.ps1');
  const notices = required(installPs1, /if \(Publish-CvcNotices -ShareDir \$ShareDir -Staged \$NoticesStaged\)/, 'the call that installs the notices', 'install.ps1');
  const moves = required(installPs1, /Publish-CvcBinaries -InstallDir \$InstallDir -MoveOrder \$MoveOrder -Prepared \$Prepared/, 'the Publish-CvcBinaries call', 'install.ps1');
  assert.ok(staged.index < notices.index && notices.index < moves.index);
});

test('neither installer lets a problem with the notices stop the binaries', () => {
  // install.sh calls publish_notices only as an `if` condition, so a failure
  // takes the else branch instead of exiting under `set -e`.
  assert.doesNotMatch(installSh, /^\s*publish_notices(?!\w|\(\))/m, 'publish_notices must not be called bare: under set -e a failure would end the install');
  required(installSh, /if publish_notices; then\n\s*NOTICES_INSTALLED=\$NOTICES_STAGED\n\s*else\n\s*NOTICES_FAILED=\$NOTICES_STAGED\n/, 'the guarded call to publish_notices', 'install.sh');
  // install.ps1 tests Publish-CvcNotices as an `if` condition, and the
  // function catches everything it can throw.
  required(installPs1, /function Publish-CvcNotices[\s\S]*?\n    catch \{\n        Write-Warning [^\n]*\n        return \$false\n    \}/, 'the catch block of Publish-CvcNotices', 'install.ps1');
});
