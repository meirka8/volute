#!/usr/bin/env node
// Produces each npm package name of the launcher from the one source directory.
//
// The launcher is published under two names: @volute_cvc/cvc-mcp (the name MCP
// client configurations already use) and @volute_cvc/cvc (the name for the CLI).
// npm/cvc-mcp stays the single source. This script is the only place that knows
// how the names differ: the name, which README the package page shows, and
// optionally the description npm lists the package under. It stages a copy of
// the source with that transformation applied and prints the copy's directory,
// so a release and CI publish, pack and dry-run exactly the same thing. It also
// owns the list of names (and their publish order), so neither workflow
// hard-codes them.
//
// Why a copy rather than editing package.json in place: both names are
// published from one checkout, and an in-place edit would leave the source
// changed after a failed or interrupted release step.
//
// The copy mirrors the repository layout, <root>/THIRD-PARTY-NOTICES.md and
// <root>/npm/<name>/, so the unchanged prepack/postpack hook
// (stage-third-party-notices.mjs, which reads ../../THIRD-PARTY-NOTICES.md)
// works in the copy exactly as it does in the source.
//
// Usage:
//   node scripts/stage-package.mjs --list
//   node scripts/stage-package.mjs <package-name> [--version <semver>] [--out <empty-dir>]
//
// `--list` prints the names in publish order, one per line. Otherwise the staged
// package directory is printed. `--version` stamps the staged manifest (CI uses a
// throwaway prerelease); without it the version is the source's. Remove the stage
// root (two levels above the printed directory) when done.
import { copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SOURCE_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/**
 * Every published name, in publish order, with the README its package page
 * shows (a path inside the source directory) and, optionally, its own
 * `description`. The source manifest carries the first name, with its own
 * description and README, so that entry sets no description; every other name is
 * derived from the source, and an entry without a description keeps the source's.
 */
export const PACKAGES = Object.freeze([
  Object.freeze({ name: '@volute_cvc/cvc-mcp', readme: 'README.md' }),
  Object.freeze({
    name: '@volute_cvc/cvc',
    readme: 'readmes/cvc.md',
    description: 'Command-line launcher for Volute CVC (Cognitive Version Control): provides the cvc, cvc-mcp and cvc-lsp commands',
  }),
]);

export const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*)?(?:\+[0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*)?$/;

// Top-level entries of the source that are not part of a package: not shipped,
// or (readmes/, README*) replaced by the chosen README, or
// (THIRD-PARTY-NOTICES.md) a generated file that prepack stages itself and
// refuses to overwrite. Everything else is copied, so the manifest's `files`
// list alone decides the tarball, as it does in the source directory.
const NOT_COPIED = new Set(['node_modules', '.git', 'test', 'readmes', 'THIRD-PARTY-NOTICES.md', 'test-local-install.sh']);
const README_NAME = /^readme(\.|$)/i;

export function packageNames() {
  return PACKAGES.map(entry => entry.name);
}

function regularFile(file, label) {
  if (!existsSync(file)) throw new Error(`${label} is missing: ${file}`);
  const stat = lstatSync(file);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} must be a regular file: ${file}`);
}

function emptyOrMissing(dir) {
  return !existsSync(dir) || (lstatSync(dir).isDirectory() && readdirSync(dir).length === 0);
}

/** Serializes `manifest` the way `raw` was written: same indent, same trailing newline. */
function serialize(manifest, raw) {
  const indent = /^([ \t]+)"/m.exec(raw)?.[1] ?? '  ';
  return JSON.stringify(manifest, null, indent) + (raw.endsWith('\n') ? '\n' : '');
}

/**
 * Stages `name` and returns { root, dir, manifest }: `dir` is the package
 * directory, `root` the stage root to remove afterwards. Options: `version`
 * (stamped into the staged manifest), `out` (an absent or empty directory to use
 * as the stage root instead of a fresh temporary one) and, for tests, `sourceDir`.
 */
export function stagePackage(name, { version, out, sourceDir = SOURCE_DIR } = {}) {
  const entry = PACKAGES.find(candidate => candidate.name === name);
  if (!entry) throw new Error(`Unknown package ${JSON.stringify(name)}; expected one of ${packageNames().join(', ')}`);
  if (version !== undefined && !SEMVER.test(version)) throw new Error(`--version must be a semantic version; received ${JSON.stringify(version)}`);

  const repoRoot = resolve(sourceDir, '..', '..');
  const raw = readFileSync(join(sourceDir, 'package.json'), 'utf8');
  const manifest = JSON.parse(raw);
  if (manifest.name !== PACKAGES[0].name) throw new Error(`The source manifest must be named ${PACKAGES[0].name}, found ${JSON.stringify(manifest.name)}`);
  if (serialize(manifest, raw) !== raw) throw new Error('The source package.json must round-trip through JSON unchanged, so a staged manifest differs only in name and version');

  regularFile(join(repoRoot, 'THIRD-PARTY-NOTICES.md'), 'root THIRD-PARTY-NOTICES.md');
  regularFile(join(sourceDir, entry.readme), `README for ${name}`);

  let root;
  if (out === undefined) {
    root = mkdtempSync(join(tmpdir(), 'cvc-npm-stage-'));
  } else {
    root = resolve(out);
    if (!emptyOrMissing(root)) throw new Error(`--out must be absent or an empty directory: ${root}`);
    mkdirSync(root, { recursive: true });
  }
  const dir = join(root, 'npm', name.split('/').pop());
  mkdirSync(dir, { recursive: true });

  copyFileSync(join(repoRoot, 'THIRD-PARTY-NOTICES.md'), join(root, 'THIRD-PARTY-NOTICES.md'));
  cpSync(sourceDir, dir, {
    recursive: true,
    filter: source => {
      if (lstatSync(source).isSymbolicLink()) throw new Error(`Refusing to stage a symbolic link: ${source}`);
      const top = relative(sourceDir, source).split(sep)[0];
      return !(NOT_COPIED.has(top) || README_NAME.test(top) || top.endsWith('.tgz'));
    },
  });
  copyFileSync(join(sourceDir, entry.readme), join(dir, 'README.md'));

  const staged = {
    ...manifest,
    name,
    ...(entry.description === undefined ? {} : { description: entry.description }),
    ...(version === undefined ? {} : { version }),
  };
  const unchanged = ['name', 'description', 'version'].every(key => staged[key] === manifest[key]);
  writeFileSync(join(dir, 'package.json'), unchanged ? raw : serialize(staged, raw));
  return { root, dir, manifest: staged };
}

function parseArguments(argv) {
  const options = { list: false, name: undefined, version: undefined, out: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--list') options.list = true;
    else if (argument === '--version') options.version = argv[++index];
    else if (argument === '--out') options.out = argv[++index];
    else if (argument.startsWith('-') || options.name !== undefined) throw new Error(`Unknown argument ${JSON.stringify(argument)}`);
    else options.name = argument;
  }
  if (options.list === (options.name !== undefined)) throw new Error('Usage: stage-package.mjs --list | <package-name> [--version <semver>] [--out <dir>]');
  if (options.list && (options.version !== undefined || options.out !== undefined)) throw new Error('--list takes no other arguments');
  return options;
}

function main(argv) {
  const options = parseArguments(argv);
  if (options.list) console.log(packageNames().join('\n'));
  else console.log(stagePackage(options.name, { version: options.version, out: options.out }).dir);
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
