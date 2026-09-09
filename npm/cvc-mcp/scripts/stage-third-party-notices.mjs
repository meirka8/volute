#!/usr/bin/env node
import { constants, copyFileSync, existsSync, lstatSync, readFileSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, resolve } from 'node:path';

const packageRoot = resolve(dirname(new URL(import.meta.url).pathname), '..');
const source = resolve(packageRoot, '..', '..', 'THIRD-PARTY-NOTICES.md');
const destination = resolve(packageRoot, 'THIRD-PARTY-NOTICES.md');
const digest = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const regular = (file, label) => {
  if (!existsSync(file)) throw new Error(`${label} is missing`);
  const stat = lstatSync(file);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} must be a regular file`);
};

if (process.argv[2] === 'stage') {
  regular(source, 'root THIRD-PARTY-NOTICES.md');
  if (existsSync(destination)) throw new Error('refusing to overwrite staged THIRD-PARTY-NOTICES.md; run cleanup first');
  copyFileSync(source, destination, constants.COPYFILE_EXCL);
  regular(destination, 'staged THIRD-PARTY-NOTICES.md');
  if (digest(source) !== digest(destination)) throw new Error('staged notice digest mismatch');
} else if (process.argv[2] === 'cleanup') {
  if (existsSync(destination)) { regular(destination, 'staged THIRD-PARTY-NOTICES.md'); rmSync(destination); }
} else {
  throw new Error('Usage: stage-third-party-notices.mjs <stage|cleanup>');
}
