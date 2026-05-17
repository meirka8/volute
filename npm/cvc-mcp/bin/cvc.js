#!/usr/bin/env node

const os = require('os');
const path = require('path');
const fs = require('fs');
const https = require('https');
const child_process = require('child_process');

const CACHE_DIR = path.join(os.homedir(), '.cvc', 'mcp-cache');

function getPlatformDetails() {
  const platform = os.platform();
  const arch = os.arch();

  let osTag = '';
  let archTag = '';

  // Determine OS Tag
  if (platform === 'linux') {
    osTag = 'unknown-linux-gnu';
  } else if (platform === 'darwin') {
    osTag = 'apple-darwin';
  } else if (platform === 'win32') {
    osTag = 'pc-windows-msvc';
  } else {
    console.error(`Unsupported OS: ${platform}`);
    process.exit(1);
  }

  // Determine Arch Tag
  if (arch === 'x64') {
    archTag = 'x86_64';
  } else if (arch === 'arm64' || arch === 'aarch64') {
    archTag = 'aarch64';
  } else {
    console.error(`Unsupported Architecture: ${arch}`);
    process.exit(1);
  }

  return { platform, osTag, archTag };
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    https.get(url, (response) => {
      if (response.statusCode === 301 || response.statusCode === 302) {
        return download(response.headers.location, dest).then(resolve).catch(reject);
      }
      if (response.statusCode !== 200) {
        return reject(new Error(`Failed to download ${url}: ${response.statusCode}`));
      }
      response.pipe(file);
      file.on('finish', () => {
        file.close(resolve);
      });
    }).on('error', (err) => {
      fs.unlink(dest, () => reject(err));
    });
  });
}

function extract(archivePath, destDir, platform) {
  if (platform === 'win32') {
    // Basic unzipping on windows using powershell
    child_process.execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${destDir}' -Force"`, { stdio: 'inherit' });
  } else {
    // Assuming tar is available on Unix
    child_process.execSync(`tar -xzf "${archivePath}" -C "${destDir}"`, { stdio: 'inherit' });
  }
}

async function main() {
  const { platform, osTag, archTag } = getPlatformDetails();
  const ext = platform === 'win32' ? 'zip' : 'tar.gz';
  const binName = platform === 'win32' ? 'cvc.exe' : 'cvc';
  const assetName = `cvc-${archTag}-${osTag}.${ext}`;
  const downloadUrl = `https://cvc.dev/api/download/${assetName}`;
  
  const binPath = path.join(CACHE_DIR, binName);

  if (!fs.existsSync(binPath)) {
    console.error(`CVC CLI not found locally. Downloading from ${downloadUrl}...`);
    fs.mkdirSync(CACHE_DIR, { recursive: true });
    const tmpArchive = path.join(CACHE_DIR, assetName);

    try {
      await download(downloadUrl, tmpArchive);
      console.error('Extracting...');
      extract(tmpArchive, CACHE_DIR, platform);
      fs.unlinkSync(tmpArchive); // Clean up archive
      if (platform !== 'win32') {
        fs.chmodSync(binPath, 0o755);
        // Also ensure MCP and LSP are executable since they are extracted together
        const mcpPath = path.join(CACHE_DIR, 'cvc-mcp');
        const lspPath = path.join(CACHE_DIR, 'cvc-lsp');
        if (fs.existsSync(mcpPath)) fs.chmodSync(mcpPath, 0o755);
        if (fs.existsSync(lspPath)) fs.chmodSync(lspPath, 0o755);
      }
      console.error('Download and extraction complete.');
    } catch (e) {
      console.error(`Failed to install CVC CLI: ${e.message}`);
      process.exit(1);
    }
  }

  // Execute the binary
  const args = process.argv.slice(2);
  const child = child_process.spawn(binPath, args, {
    stdio: 'inherit'
  });

  child.on('close', (code) => {
    process.exit(code);
  });
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
