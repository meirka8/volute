#!/usr/bin/env node
/** Dependency-free check of the Claude Code plugin and its marketplace.
 *
 * "Claude Code plugin" here means the plugin that ships in plugins/claude-code
 * and is installed with `/plugin install cvc@volute`; it is unrelated to the VS
 * Code extension in cvc-plugin/. The script validates the three files Claude
 * Code reads (.claude-plugin/marketplace.json, the plugin's
 * .claude-plugin/plugin.json, and its hooks/hooks.json) against the contract
 * this repository documents, without needing Claude Code itself.
 *
 *   node .github/scripts/check-claude-plugin.mjs               # check this repository
 *   node .github/scripts/check-claude-plugin.mjs --root <dir>  # check another tree
 *   node .github/scripts/check-claude-plugin.mjs --self-test   # test the checker
 *
 * The catalog is read from the default branch on every marketplace update, so it
 * is limited to a fixed set of fields: an entry that carried plugin-manifest fields
 * such as inline hooks would run on users' machines and bypass the release-tag pin.
 * The marketplace entry fetches the plugin with a git-subdir source pinned to a
 * release tag, so only a release publishes plugin content. This script requires
 * that tag to be "v" + the plugin manifest version, and that version to equal
 * cvc-cli's, so a pull request cannot drift from the CLI it needs. The release
 * gate, check-release-version.mjs, requires the same ref to equal the pushed tag.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';

const MARKETPLACE_NAME = 'volute';
const PLUGIN_NAME = 'cvc';
const MARKETPLACE_FILE = '.claude-plugin/marketplace.json';
const CARGO_FILE = 'cvc-cli/Cargo.toml';
const PLUGIN_PATH = 'plugins/claude-code';
const REPOSITORY_URL = 'https://github.com/meirka8/volute.git';
const SOURCE_KEYS = ['source', 'url', 'path', 'ref'];
// The catalog is read from the default branch on every marketplace update, so it may carry only these
// fields. A marketplace entry can also carry plugin-manifest fields (hooks, mcpServers, commands, agents,
// skills, lspServers, outputStyles, strict, dependencies); one of those would run on users' machines
// after an update and bypass the release-tag pin.
const MARKETPLACE_KEYS = ['name', 'description', 'owner', 'plugins'];
const OWNER_KEYS = ['name', 'url'];
// A full https URL clones a public repository without credentials; `owner/repo` would need an SSH key.
const REF_SYNTAX = /^v\d+\.\d+\.\d+$/;
// Claude Code does not read plugin.json before it installs a git-subdir plugin, so the entry repeats these.
const DISPLAY_FIELDS = ['description', 'author', 'homepage', 'repository', 'license', 'keywords'];
const ENTRY_KEYS = ['name', 'source', ...DISPLAY_FIELDS];
// Claude Code rejects these marketplace names, and anything starting `claudeai-`.
const RESERVED_NAMES = new Set([
  'claude-code-marketplace', 'anthropic-plugins', 'claude-community', 'claude-plugins-official',
  'healthcare', 'anthropic-plugin-directory', 'npm', 'pip', 'uv', 'cargo', 'github', 'gh',
]);
const NAME_SYNTAX = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
// Same stable-only SemVer the release version gate accepts.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
// Every ingest hook goes through run-cvc.sh, which runs only a cvc from an absolute PATH directory.
const WRAPPER_SCRIPT = 'scripts/run-cvc.sh';
const INGEST_ARGUMENTS = 'ingest claude-code --hook';
// The commands name /bin/sh, so a `sh` in the project cannot take its place. Claude Code treats exit
// status 2 as blocking, and some sh builds exit 2 when they cannot open the script, so 2 becomes 1.
const INGEST_HOOK = Object.freeze({ type: 'command', command: `/bin/sh "\${CLAUDE_PLUGIN_ROOT}/${WRAPPER_SCRIPT}" ${INGEST_ARGUMENTS}; s=$?; [ "$s" -ne 2 ] || s=1; exit "$s"`, timeout: 30 });
const INGEST_EVENTS = ['PostToolUse', 'Stop', 'SessionEnd'];
const NOT_FOUND_MESSAGE = 'cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)';
const SESSION_START_MATCHER = 'startup|resume';
// SessionStart only checks that the CLI is present and new enough; it never ingests.
const SESSION_START_SCRIPT = 'scripts/check-cvc.sh';
const SESSION_START_HOOK = Object.freeze({ type: 'command', command: `/bin/sh "\${CLAUDE_PLUGIN_ROOT}/${SESSION_START_SCRIPT}"`, timeout: 30 });

const isObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const nonEmpty = value => typeof value === 'string' && value.trim() !== '';
const show = value => JSON.stringify(value);

function validate(root) {
  const problems = [];
  const rel = path => relative(root, path).split(sep).join('/');
  const fail = (file, message) => problems.push({ file, message });

  function readJson(path) {
    if (!existsSync(path)) { fail(rel(path), 'file is missing.'); return undefined; }
    try { return JSON.parse(readFileSync(path, 'utf8')); } catch (error) { fail(rel(path), `is not valid JSON: ${error.message}`); return undefined; }
  }

  // The CLI manifest is the source of truth for the version and licence.
  function cargoFields() {
    const path = join(root, CARGO_FILE);
    if (!existsSync(path)) { fail(CARGO_FILE, 'file is missing.'); return {}; }
    const text = readFileSync(path, 'utf8');
    const versions = [...text.matchAll(/^version\s*=\s*"([^"]+)"\s*$/gm)];
    const licenses = [...text.matchAll(/^license\s*=\s*"([^"]+)"\s*$/gm)];
    if (versions.length !== 1) fail(CARGO_FILE, 'must contain exactly one package version.');
    return { version: versions.length === 1 ? versions[0][1] : undefined, license: licenses.length === 1 ? licenses[0][1] : undefined };
  }

  // Fails on every key outside `allowed` (keys in `except` have their own, more specific check).
  function onlyKeys(file, label, object, allowed, reason, except = []) {
    for (const key of Object.keys(object)) {
      if (!allowed.includes(key) && !except.includes(key)) fail(file, `${label} has unexpected key ${show(key)}; only ${allowed.map(show).join(', ')} are allowed${reason}.`);
    }
  }

  function checkName(file, label, name) {
    if (!nonEmpty(name)) { fail(file, `${label} must be a non-empty string.`); return false; }
    if (!NAME_SYNTAX.test(name)) { fail(file, `${label} ${show(name)} may contain only letters, digits, ".", "_" and "-", and must start with a letter or digit.`); return false; }
    return true;
  }

  // The entry's `source` must be a git-subdir object pinned to a release tag. Returns the plugin
  // directory when the entry is usable enough to continue.
  function checkSource(entry) {
    const field = '`plugins[0].source`';
    if (!isObject(entry.source)) {
      fail(MARKETPLACE_FILE, `${field} must be a git-subdir object pinned to a release tag, not ${show(entry.source)}: a relative path would serve whatever is on the default branch.`);
      return undefined;
    }
    const source = entry.source;
    for (const key of Object.keys(source)) {
      if (key === 'sha') fail(MARKETPLACE_FILE, `${field} must not set \`sha\`: the release commit is the tagged commit, so it cannot name its own SHA.`);
      else if (!SOURCE_KEYS.includes(key)) fail(MARKETPLACE_FILE, `${field} has unexpected key ${show(key)}; only ${SOURCE_KEYS.map(show).join(', ')} are allowed.`);
    }
    if (source.source !== 'git-subdir') fail(MARKETPLACE_FILE, `${field}.source must be "git-subdir"; received ${show(source.source)}.`);
    if (source.url !== REPOSITORY_URL) fail(MARKETPLACE_FILE, `${field}.url must be exactly ${show(REPOSITORY_URL)} (a full https URL, not owner/repo); received ${show(source.url)}.`);
    if (source.path !== PLUGIN_PATH) fail(MARKETPLACE_FILE, `${field}.path must be exactly ${show(PLUGIN_PATH)}; received ${show(source.path)}.`);
    if (typeof source.ref !== 'string' || !REF_SYNTAX.test(source.ref)) fail(MARKETPLACE_FILE, `${field}.ref must match ${REF_SYNTAX} (a release tag, never a branch); received ${show(source.ref)}.`);

    const pluginDir = resolve(root, PLUGIN_PATH);
    if (!existsSync(pluginDir) || !statSync(pluginDir).isDirectory()) { fail(MARKETPLACE_FILE, `${field}.path ${show(PLUGIN_PATH)} does not exist as a directory in this tree.`); return undefined; }
    return pluginDir;
  }

  // Returns { entry, pluginDir } when the marketplace is usable enough to continue.
  function checkMarketplace() {
    const path = join(root, MARKETPLACE_FILE);
    const marketplace = readJson(path);
    if (marketplace === undefined) return undefined;
    if (!isObject(marketplace)) { fail(MARKETPLACE_FILE, 'must be a JSON object.'); return undefined; }
    onlyKeys(MARKETPLACE_FILE, 'the marketplace', marketplace, MARKETPLACE_KEYS, ': the catalog is read from the default branch on every update, so it is limited to the fields this check pins');

    if (checkName(MARKETPLACE_FILE, '`name`', marketplace.name)) {
      if (RESERVED_NAMES.has(marketplace.name) || marketplace.name.startsWith('claudeai-')) fail(MARKETPLACE_FILE, `\`name\` ${show(marketplace.name)} is reserved by Claude Code.`);
      // The documented install command is `cvc@volute`, so renaming breaks users.
      if (marketplace.name !== MARKETPLACE_NAME) fail(MARKETPLACE_FILE, `\`name\` must be ${show(MARKETPLACE_NAME)} because the documented install id is ${PLUGIN_NAME}@${MARKETPLACE_NAME}; received ${show(marketplace.name)}.`);
    }

    // `claude plugin validate` warns about a missing marketplace description.
    if (!nonEmpty(marketplace.description)) fail(MARKETPLACE_FILE, '`description` must be a non-empty string.');

    if (!isObject(marketplace.owner)) fail(MARKETPLACE_FILE, '`owner` must be an object.');
    else {
      if (!nonEmpty(marketplace.owner.name)) fail(MARKETPLACE_FILE, '`owner.name` must be a non-empty string.');
      if ('url' in marketplace.owner && !nonEmpty(marketplace.owner.url)) fail(MARKETPLACE_FILE, '`owner.url` must be a non-empty string when present.');
      onlyKeys(MARKETPLACE_FILE, '`owner`', marketplace.owner, OWNER_KEYS, '');
    }

    if (!Array.isArray(marketplace.plugins) || marketplace.plugins.length !== 1) {
      fail(MARKETPLACE_FILE, `\`plugins\` must list exactly one plugin (${show(PLUGIN_NAME)}); received ${Array.isArray(marketplace.plugins) ? marketplace.plugins.length : typeof marketplace.plugins}.`);
      return undefined;
    }
    const entry = marketplace.plugins[0];
    if (!isObject(entry)) { fail(MARKETPLACE_FILE, '`plugins[0]` must be an object.'); return undefined; }
    onlyKeys(MARKETPLACE_FILE, '`plugins[0]`', entry, ENTRY_KEYS, ": an entry can carry plugin-manifest fields (hooks, mcpServers, commands, agents, skills, strict, ...) that would run on users' machines after a marketplace update and bypass the release-tag pin", ['version']);
    if (checkName(MARKETPLACE_FILE, '`plugins[0].name`', entry.name) && entry.name !== PLUGIN_NAME) fail(MARKETPLACE_FILE, `\`plugins[0].name\` must be ${show(PLUGIN_NAME)}; received ${show(entry.name)}.`);
    // The manifest version wins over a marketplace version, so a second copy can only go stale.
    if ('version' in entry) fail(MARKETPLACE_FILE, '`plugins[0]` must not set `version`; the plugin manifest is the single place the version lives.');

    const pluginDir = checkSource(entry);
    return pluginDir ? { entry, pluginDir } : undefined;
  }

  // The entry repeats the manifest's display fields and names the tag of the version it ships.
  function checkEntryAgainstManifest(entry, manifestFile, manifest) {
    for (const field of DISPLAY_FIELDS) {
      if (!isDeepStrictEqual(entry[field], manifest[field])) fail(MARKETPLACE_FILE, `\`plugins[0].${field}\` must equal \`${field}\` in ${manifestFile}, which Claude Code does not read before it installs a git-subdir plugin; received ${show(entry[field])}, expected ${show(manifest[field])}.`);
    }
    if (typeof manifest.version === 'string' && SEMVER.test(manifest.version) && isObject(entry.source) && typeof entry.source.ref === 'string' && REF_SYNTAX.test(entry.source.ref) && entry.source.ref !== `v${manifest.version}`) {
      fail(MARKETPLACE_FILE, `\`plugins[0].source.ref\` must be ${show(`v${manifest.version}`)} (v + the plugin manifest version in ${manifestFile}); received ${show(entry.source.ref)}.`);
    }
  }

  // Returns the manifest and its path when it parsed.
  function checkManifest(pluginDir, entryName, cargo) {
    const path = join(pluginDir, '.claude-plugin', 'plugin.json');
    const file = rel(path);
    const manifest = readJson(path);
    if (manifest === undefined) return undefined;
    if (!isObject(manifest)) { fail(file, 'must be a JSON object.'); return undefined; }

    if (checkName(file, '`name`', manifest.name) && manifest.name !== entryName) fail(file, `\`name\` ${show(manifest.name)} must equal the marketplace entry name ${show(entryName)}, or the install id changes.`);
    if (typeof manifest.version !== 'string' || !SEMVER.test(manifest.version)) fail(file, `\`version\` must be stable MAJOR.MINOR.PATCH SemVer; received ${show(manifest.version)}.`);
    else if (cargo.version !== undefined && manifest.version !== cargo.version) fail(file, `\`version\` ${show(manifest.version)} does not match ${CARGO_FILE} version ${show(cargo.version)}. The plugin version tracks the CLI release it requires.`);
    if (!nonEmpty(manifest.license)) fail(file, '`license` must be a non-empty SPDX string.');
    else if (cargo.license !== undefined && manifest.license !== cargo.license) fail(file, `\`license\` ${show(manifest.license)} does not match ${CARGO_FILE} license ${show(cargo.license)}.`);
    if (!nonEmpty(manifest.description)) fail(file, '`description` must be a non-empty string.');
    if ('author' in manifest && !(isObject(manifest.author) && nonEmpty(manifest.author.name))) fail(file, '`author` must be an object with a non-empty `name`.');
    for (const key of ['homepage', 'repository']) if (key in manifest && !/^https:\/\//.test(manifest[key] ?? '')) fail(file, `\`${key}\` must be an https URL string.`);
    if ('keywords' in manifest && !(Array.isArray(manifest.keywords) && manifest.keywords.every(nonEmpty))) fail(file, '`keywords` must be an array of non-empty strings.');
    // hooks/hooks.json is loaded by default; declaring it again would load it twice.
    if ('hooks' in manifest) fail(file, '`hooks` must not be declared: the plugin keeps its hooks in hooks/hooks.json, which Claude Code loads by default.');
    return { manifest, file };
  }

  // Runs the hook commands the way Claude Code would, with a PATH that holds only the stub's
  // directory (the commands name /bin/sh, so no `sh` is needed on it), and with CLAUDE_PROJECT_DIR
  // set to the session's (real) project directory, which Claude Code gives hooks. The stub `cvc`
  // logs its arguments, standard input and CLAUDE_PROJECT_DIR.
  // `sessionStart` and `ingest` are the commands to exercise, or undefined to skip that part.
  function checkBehaviour(file, pluginDir, sessionStart, ingest) {
    if (process.platform === 'win32' || !existsSync('/bin/sh')) return; // POSIX sh is needed to run them
    const base = mkdtempSync(join(tmpdir(), 'cvc-hook-'));
    const expectedArgv = `${INGEST_ARGUMENTS}\n`;
    // Hook mode routes by CLAUDE_PROJECT_DIR, so the probe must override the session's value.
    const probeProjectDir = '/nonexistent/cvc-plugin-probe';
    const expectedProjectDir = `CLAUDE_PROJECT_DIR=${probeProjectDir}\n`;
    const projectDir = join(base, 'project');
    const payload = '{"hook_event_name":"PostToolUse"}';
    // The stub prints the variable it was started with (a literal shell word, expanded by the stub).
    const stubEnvLine = '"CLAUDE_PROJECT_DIR=${CLAUDE_PROJECT_DIR-UNSET}"';
    mkdirSync(projectDir, { recursive: true });

    // Runs `command` under `sh -c`. Returns { stdout, argv, projectDir, stdin, bin } after checking the
    // exit status, stderr and that nothing planted in the current directory ran, or undefined on failure.
    //   stubExit:     exit status of the stub `cvc`, or undefined for no stub
    //   pathEntries:  PATH entries placed before the stub's absolute directory ('' is an empty entry)
    //   pathOverride: the whole PATH, instead of pathEntries and the stub's directory
    //   stubDir:      where the stub lives, instead of its own directory
    //   hostile:      names (`cvc`, `sh`) planted in the working directory, each leaving a marker if it runs
    //   status:       the exit status, or several that are acceptable (dash exits 2 and bash 127 for a missing script)
    //   stderr:       the exact stderr of a failing run, or null for any
    function run(name, what, label, stubExit, options) {
      const { command, root = pluginDir, pathEntries = [], pathOverride, stubDir, hostile = [], input = '', status = 0, stderr = '', cwd: cwdOption, env: extraEnv = {} } = options;
      const dir = join(base, name);
      const bin = join(dir, 'bin');
      const cwd = cwdOption ?? (hostile.length > 0 ? join(dir, 'cwd') : projectDir);
      const logs = { argv: join(dir, 'argv.log'), env: join(dir, 'env.log'), stdin: join(dir, 'stdin.log') };
      mkdirSync(bin, { recursive: true });
      mkdirSync(cwd, { recursive: true });
      if (stubExit !== undefined) {
        mkdirSync(stubDir ?? bin, { recursive: true });
        writeFileSync(join(stubDir ?? bin, 'cvc'), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${logs.argv}'\nprintf '%s\\n' ${stubEnvLine} >> '${logs.env}'\nIFS= read -r _line || :\nprintf '%s\\n' "$_line" >> '${logs.stdin}'\nexit ${stubExit}\n`, { mode: 0o755 });
      }
      const markers = hostile.map(kind => [kind, join(dir, `hostile-${kind}-ran`)]);
      for (const [kind, marker] of markers) writeFileSync(join(cwd, kind), `#!/bin/sh\n: > '${marker}'\nexit 0\n`, { mode: 0o755 });
      const env = { PATH: pathOverride ?? [...pathEntries, bin].join(':'), CLAUDE_PLUGIN_ROOT: root, CLAUDE_PROJECT_DIR: projectDir, ...extraEnv };
      const result = spawnSync('/bin/sh', ['-c', command], { cwd, env, input, encoding: 'utf8', timeout: 15_000 });
      if (result.error) { fail(file, `${what} ${label} could not run: ${result.error.message}`); return undefined; }
      for (const [kind, marker] of markers) {
        if (existsSync(marker)) { fail(file, `${what} ${label} must not run a ${kind} found through a relative PATH entry; the ${kind} in the current directory ran.`); return undefined; }
      }
      const statuses = [status].flat();
      if (!statuses.includes(result.status)) { fail(file, `${what} ${label} must exit ${statuses.join(' or ')}; exited ${result.status}${result.stderr ? `: ${result.stderr.trim()}` : ''}.`); return undefined; }
      if (stderr !== null && (statuses.includes(0) ? result.stderr !== '' : result.stderr.trim() !== stderr)) { fail(file, statuses.includes(0) ? `${what} ${label} must write nothing to stderr; wrote ${show(result.stderr)}.` : `${what} ${label} must print ${show(stderr)} on stderr; wrote ${show(result.stderr)}.`); return undefined; }
      const read = path => (existsSync(path) ? readFileSync(path, 'utf8') : '');
      return { stdout: result.stdout, argv: read(logs.argv), projectDir: read(logs.env), stdin: read(logs.stdin), bin };
    }
    // The probe must run cvc once, with the hook-mode arguments, and with the probe's own project directory.
    function checkProbe(who, result) {
      if (result.argv !== expectedArgv) fail(file, `${who} must probe cvc exactly once with \`${expectedArgv.trim()}\`; the stub saw ${show(result.argv)}.`);
      else if (result.projectDir !== expectedProjectDir) fail(file, `${who} must run the probe with ${expectedProjectDir.trim()}, so it does not depend on the session's project; the stub saw ${show(result.projectDir)}.`);
    }
    // One JSON line whose `systemMessage` matches; the exact wording is pinned by the Rust tests.
    // `avoid` is a pattern the message must not match.
    function message(label, stdout, pattern, mention, avoid) {
      const lines = stdout.split('\n').filter(line => line.trim() !== '');
      let parsed;
      try { parsed = lines.length === 1 ? JSON.parse(lines[0]) : undefined; } catch { parsed = undefined; }
      if (!isObject(parsed) || !nonEmpty(parsed.systemMessage)) fail(file, `SessionStart command ${label} must print exactly one JSON object with a \`systemMessage\`; printed ${show(stdout)}.`);
      else if (!pattern.test(parsed.systemMessage)) fail(file, `SessionStart command ${label} must print a \`systemMessage\` that mentions ${mention}; printed ${show(parsed.systemMessage)}.`);
      else if (avoid?.test(parsed.systemMessage)) fail(file, `SessionStart command ${label} must not blame a relative PATH entry; printed ${show(parsed.systemMessage)}.`);
    }
    try {
      if (sessionStart !== undefined) {
        const what = 'SessionStart command';
        const start = (name, label, stubExit, options = {}) => run(name, what, label, stubExit, { command: sessionStart, ...options });

        const missing = start('missing', 'with no cvc on PATH', undefined);
        if (missing) message('with no cvc on PATH', missing.stdout, /PATH/, 'PATH');

        const current = start('current', 'with a cvc that accepts the probe', 0);
        if (current) {
          if (current.stdout !== '') fail(file, `SessionStart command with a cvc that accepts the probe must print nothing; printed ${show(current.stdout)}.`);
          checkProbe(what, current);
        }

        // An older cvc rejects the probe with exit status 1.
        const old = start('old', 'with a cvc that fails the probe', 1);
        if (old) {
          message('with a cvc that fails the probe', old.stdout, /older/, '"older"');
          checkProbe(what, old);
        }

        // Any other failure means the cvc could not be run at all (for example an npm-installed cvc
        // whose node is not on this PATH), and 126 and 127 must not be mistaken for a missing cvc.
        for (const code of [127, 126]) {
          const label = `with a cvc that exits ${code} when run`;
          const unrunnable = start(`unrunnable-${code}`, label, code);
          if (unrunnable) message(label, unrunnable.stdout, /could not be run/, '"could not be run"', /relative/);
        }

        // Claude Code keeps installed plugins under a cache directory that can contain a space
        // (a home directory such as "Jane Doe"), and the command expands CLAUDE_PLUGIN_ROOT in the
        // shell. Run the same check from a copy of the plugin at such a path.
        const spaced = join(base, 'plugin root');
        cpSync(pluginDir, spaced, { recursive: true });
        const spacedLabel = 'in a CLAUDE_PLUGIN_ROOT containing a space, with a cvc that fails the probe';
        const inSpace = start('spaced', spacedLabel, 1, { root: spaced });
        if (inSpace) {
          message(spacedLabel, inSpace.stdout, /older/, '"older"');
          checkProbe(`${what} ${spacedLabel}`, inSpace);
        }

        // A relative or empty PATH entry is the current directory, which is the project in a hook. A
        // cvc planted there must never run, and an absolute cvc later in PATH must still be used.
        const shadowedLabel = 'with relative PATH entries, a cvc in the current directory and an absolute cvc that accepts the probe';
        const shadowed = start('shadowed', shadowedLabel, 0, { pathEntries: ['.', ''], hostile: ['cvc'] });
        if (shadowed) {
          if (shadowed.stdout !== '') fail(file, `SessionStart command ${shadowedLabel} must print nothing; printed ${show(shadowed.stdout)}.`);
          checkProbe(`${what} ${shadowedLabel}`, shadowed);
        }
        const relativeLabel = 'with a cvc reachable only through a relative PATH entry';
        const relativeOnly = start('relative', relativeLabel, undefined, { pathEntries: ['.'], hostile: ['cvc'] });
        if (relativeOnly) message(relativeLabel, relativeOnly.stdout, /relative/, '"relative"');

        // The command and the scripts name /bin/sh, so a `sh` in the project cannot take its place.
        const shLabel = 'with a sh in the current directory and "." first on PATH';
        const shadowedSh = start('shadowed-sh', shLabel, 0, { pathEntries: ['.'], hostile: ['sh'] });
        if (shadowedSh) {
          if (shadowedSh.stdout !== '') fail(file, `SessionStart command ${shLabel} must print nothing; printed ${show(shadowedSh.stdout)}.`);
          checkProbe(`${what} ${shLabel}`, shadowedSh);
        }

        // Run by a bare file name, the script has no directory of its own to find run-cvc.sh in,
        // and must not look in the working directory, which is the project's.
        const bare = run('bare', 'SessionStart script', 'run by a bare file name', undefined, { command: '/bin/sh check-cvc.sh', cwd: join(pluginDir, 'scripts') });
        if (bare && bare.stdout !== '') fail(file, `SessionStart script run by a bare file name must do nothing; printed ${show(bare.stdout)}.`);
      }

      if (ingest !== undefined) {
        const what = 'ingest hook command';
        const absoluteLabel = 'with an absolute cvc';
        const absolute = run('ingest', what, absoluteLabel, 0, { command: ingest, input: payload });
        if (absolute) {
          if (absolute.argv !== expectedArgv) fail(file, `${what} ${absoluteLabel} must run cvc exactly once with \`${expectedArgv.trim()}\`; the stub saw ${show(absolute.argv)}.`);
          else if (absolute.stdin !== `${payload}\n`) fail(file, `${what} ${absoluteLabel} must pass its standard input to cvc unchanged; the stub read ${show(absolute.stdin)}.`);
          else if (absolute.projectDir !== `CLAUDE_PROJECT_DIR=${projectDir}\n`) fail(file, `${what} ${absoluteLabel} must pass CLAUDE_PROJECT_DIR through to cvc; the stub saw ${show(absolute.projectDir)}.`);
        }
        const strangerLabel = 'with only a cvc in a relative PATH entry';
        run('ingest-relative', what, strangerLabel, undefined, { command: ingest, pathEntries: ['.', ''], hostile: ['cvc', 'sh'], input: payload, status: 127, stderr: NOT_FOUND_MESSAGE });
        const shadowedLabel = 'with a cvc and a sh in the current directory and relative PATH entries first';
        const shadowed = run('ingest-shadowed', what, shadowedLabel, 0, { command: ingest, pathEntries: ['.', ''], hostile: ['cvc', 'sh'], input: payload });
        if (shadowed && shadowed.argv !== expectedArgv) fail(file, `${what} ${shadowedLabel} must run the absolute cvc exactly once with \`${expectedArgv.trim()}\`; the stub saw ${show(shadowed.argv)}.`);

        // A pattern character in an absolute PATH entry must not be expanded to a directory holding a cvc.
        const globLabel = 'with a PATH entry containing a glob character';
        const globRoot = join(base, 'glob', 'dirs');
        run('ingest-glob', what, globLabel, 0, { command: ingest, stubDir: join(globRoot, 'a'), pathOverride: join(globRoot, '*'), input: payload, status: 127, stderr: NOT_FOUND_MESSAGE });

        // Claude Code treats exit status 2 as blocking, so a hook must never exit 2: not when cvc
        // does, and not when the script exits 2 or sh cannot open it (a missing plugin root: dash exits 2,
        // bash 127, and the command turns 2 into 1).
        const exitTwo = join(base, 'exit 2 root');
        cpSync(pluginDir, exitTwo, { recursive: true });
        writeFileSync(join(exitTwo, WRAPPER_SCRIPT), '#!/bin/sh\nexit 2\n');
        run('ingest-exit2-script', what, 'whose run-cvc.sh exits 2', undefined, { command: ingest, root: exitTwo, input: payload, status: 1, stderr: null });
        run('ingest-exit2-stub', what, 'with a cvc that exits 2', 2, { command: ingest, input: payload, status: 1, stderr: null });
        run('ingest-no-root', what, 'with a plugin root that does not exist', 0, { command: ingest, root: join(base, 'no such plugin root'), input: payload, status: [1, 127], stderr: null });

        // Locate mode prints the path of the cvc the hooks would run, and runs nothing.
        const locate = `CVC_PLUGIN_LOCATE_ONLY=1 /bin/sh "\${CLAUDE_PLUGIN_ROOT}/${WRAPPER_SCRIPT}"`;
        const locateWhat = 'run-cvc.sh in locate mode';
        const locateLabel = 'with an absolute cvc and a cvc in the current directory';
        const located = run('locate', locateWhat, locateLabel, 0, { command: locate, pathEntries: ['.', ''], hostile: ['cvc'] });
        if (located) {
          if (located.stdout !== `${join(located.bin, 'cvc')}\n`) fail(file, `${locateWhat} ${locateLabel} must print the absolute path of the cvc and a newline; printed ${show(located.stdout)}.`);
          if (located.argv !== '') fail(file, `${locateWhat} ${locateLabel} must not run the cvc; the stub saw ${show(located.argv)}.`);
        }
        run('locate-none', locateWhat, 'with only a cvc in a relative PATH entry', undefined, { command: locate, pathEntries: ['.', ''], hostile: ['cvc'], status: 127, stderr: NOT_FOUND_MESSAGE });
      }
    } finally {
      rmSync(base, { recursive: true, force: true });
    }
  }

  function checkHooks(pluginDir) {
    const path = join(pluginDir, 'hooks', 'hooks.json');
    const file = rel(path);
    const config = readJson(path);
    if (config === undefined) return;
    if (!isObject(config)) { fail(file, 'must be a JSON object.'); return; }
    for (const key of Object.keys(config)) if (key !== 'description' && key !== 'hooks') fail(file, `has unexpected top-level key ${show(key)}; only "description" and "hooks" are allowed.`);
    if (!nonEmpty(config.description)) fail(file, '`description` must be a non-empty string.');
    if (!isObject(config.hooks)) { fail(file, '`hooks` must be an object keyed by event name.'); return; }

    const known = ['SessionStart', ...INGEST_EVENTS];
    for (const event of Object.keys(config.hooks)) if (!known.includes(event)) fail(file, `has unexpected hook event ${show(event)}; the plugin captures only ${known.join(', ')}. Update this script when adding one on purpose.`);

    // Each event must be exactly one group holding exactly one hook.
    function onlyHook(event, allowedGroupKeys) {
      const groups = config.hooks[event];
      if (!Array.isArray(groups) || groups.length !== 1 || !isObject(groups[0])) { fail(file, `hooks.${event} must be an array of exactly one hook group.`); return undefined; }
      const group = groups[0];
      for (const key of Object.keys(group)) if (!allowedGroupKeys.includes(key)) fail(file, `hooks.${event}[0] has unexpected key ${show(key)}${key === 'matcher' ? ' (this event must run for every occurrence)' : ''}.`);
      if (!Array.isArray(group.hooks) || group.hooks.length !== 1 || !isObject(group.hooks[0])) { fail(file, `hooks.${event}[0].hooks must be an array of exactly one hook.`); return undefined; }
      return { group, hook: group.hooks[0] };
    }

    let ingestExact = true;
    for (const event of INGEST_EVENTS) {
      const found = onlyHook(event, ['hooks']);
      if (found && isDeepStrictEqual(found.hook, INGEST_HOOK)) continue;
      ingestExact = false;
      if (found) fail(file, `hooks.${event}[0].hooks[0] must be exactly ${show(INGEST_HOOK)}; received ${show(found.hook)}.`);
    }

    let sessionStartCommand;
    const start = onlyHook('SessionStart', ['matcher', 'hooks']);
    if (start) {
      const { group, hook } = start;
      if (group.matcher !== SESSION_START_MATCHER) fail(file, `hooks.SessionStart[0].matcher must be ${show(SESSION_START_MATCHER)}; received ${show(group.matcher)}.`);
      if (!isDeepStrictEqual(hook, SESSION_START_HOOK)) fail(file, `hooks.SessionStart[0].hooks[0] must be exactly ${show(SESSION_START_HOOK)}; received ${show(hook)}.`);
      if (typeof hook.command === 'string') {
        if (hook.command.includes('ingest')) fail(file, 'hooks.SessionStart command must only check the CLI and must not run `ingest`.');
        else if (isDeepStrictEqual(hook, SESSION_START_HOOK)) sessionStartCommand = hook.command;
      }
    }

    // Run the commands only when they are the expected ones and their scripts ship, so one mistake is not reported twice.
    const shipped = script => existsSync(join(pluginDir, script));
    const sessionStartReady = sessionStartCommand !== undefined && shipped(SESSION_START_SCRIPT) && shipped(WRAPPER_SCRIPT);
    const ingestReady = ingestExact && shipped(WRAPPER_SCRIPT);
    if (sessionStartReady || ingestReady) checkBehaviour(file, pluginDir, sessionStartReady ? sessionStartCommand : undefined, ingestReady ? INGEST_HOOK.command : undefined);

    // Scripts referenced through ${CLAUDE_PLUGIN_ROOT} must ship inside the plugin directory.
    for (const [event, groups] of Object.entries(config.hooks)) {
      for (const group of Array.isArray(groups) ? groups : []) {
        for (const hook of Array.isArray(group?.hooks) ? group.hooks : []) {
          if (typeof hook?.command !== 'string') continue;
          for (const match of hook.command.matchAll(/\$(?:\{CLAUDE_PLUGIN_ROOT\}|CLAUDE_PLUGIN_ROOT(?![A-Za-z0-9_]))([^\s"'`;|&<>)]*)/g)) {
            const target = resolve(pluginDir, `.${match[1]}`);
            if (target !== pluginDir && !target.startsWith(pluginDir + sep)) fail(file, `hooks.${event} references ${show(match[0])}, which resolves outside the plugin directory.`);
            else if (!existsSync(target)) fail(file, `hooks.${event} references ${show(match[0])}, but ${rel(target)} does not exist.`);
          }
        }
      }
    }
  }

  const cargo = cargoFields();
  const located = checkMarketplace();
  if (located) {
    const checked = checkManifest(located.pluginDir, located.entry.name, cargo);
    if (checked) checkEntryAgainstManifest(located.entry, checked.file, checked.manifest);
    checkHooks(located.pluginDir);
  }
  return problems;
}

function report(problems) {
  const annotate = process.env.GITHUB_ACTIONS === 'true';
  const escape = text => text.replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A');
  for (const { file, message } of problems) {
    console.error(annotate ? `::error file=${file}::${escape(message)}` : `check-claude-plugin: ${file}: ${message}`);
  }
}

function selfTest() {
  const assert = (condition, message) => { if (!condition) throw new Error(`self-test failed: ${message}`); };
  const scriptFile = `${PLUGIN_PATH}/${SESSION_START_SCRIPT}`;
  const wrapperFile = `${PLUGIN_PATH}/${WRAPPER_SCRIPT}`;
  // run-cvc.sh as the contract describes it: only absolute PATH entries, no pattern expansion, the same
  // arguments and standard input, exit 127 with a fixed message when there is none, and a locate mode
  // that prints the path and runs nothing. Shell builtins only.
  const wrapperScript = `#!/bin/sh
set -f
found=
IFS=:
for dir in $PATH; do
  case $dir in
    /*) ;;
    *) continue ;;
  esac
  for name in cvc cvc.exe; do
    if [ -f "$dir/$name" ] && [ -x "$dir/$name" ]; then
      found=$dir/$name
      break 2
    fi
  done
done
if [ -z "$found" ]; then
  printf '%s\\n' '${NOT_FOUND_MESSAGE}' >&2
  exit 127
fi
if [ "$CVC_PLUGIN_LOCATE_ONLY" = 1 ]; then
  printf '%s\\n' "$found"
  exit 0
fi
exec "$found" "$@"
`;
  // check-cvc.sh as the contract describes it: it does nothing without a directory of its own, locates
  // cvc through run-cvc.sh, probes that path directly with the probe's own CLAUDE_PROJECT_DIR, and tells
  // apart a missing, a relative-only, an old and an unrunnable cvc.
  const checkScript = `#!/bin/sh
case $0 in
  */*) here=\${0%/*} ;;
  *) exit 0 ;;
esac
cvc=$(CVC_PLUGIN_LOCATE_ONLY=1 /bin/sh "$here/run-cvc.sh" 2>/dev/null)
if [ -z "$cvc" ]; then
  if command -v cvc >/dev/null 2>&1; then
    message="CVC: the cvc on PATH is reached only through a relative PATH entry, which the cvc plugin does not use."
  else
    message="CVC: cvc is not on the PATH Claude Code runs hooks with."
  fi
else
  printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe "$cvc" ${INGEST_ARGUMENTS} >/dev/null 2>&1
  status=$?
  case $status in
    0) exit 0 ;;
    1) message="CVC: the cvc on PATH is older than the plugin needs." ;;
    *) message="CVC: the cvc found on the PATH Claude Code runs hooks with could not be run, so the plugin cannot capture this session." ;;
  esac
fi
printf '{"systemMessage":"%s"}\\n' "$message"
exit 0
`;
  const editing = (text, name) => (from, to) => {
    assert(text.includes(from), `fixture ${name} must contain ${show(from)}`);
    return text.replace(from, () => to);
  };
  const edited = editing(checkScript, 'check-cvc.sh');
  const wrapperEdited = editing(wrapperScript, 'run-cvc.sh');
  const baseline = () => {
    const author = { name: 'Volute' };
    const display = { description: 'Captures sessions.', author, homepage: 'https://example.test/volute', repository: 'https://example.test/volute', license: 'Apache-2.0', keywords: ['cvc'] };
    return {
      cargo: '[package]\nname = "cvc-cli"\nversion = "1.2.3"\nlicense = "Apache-2.0"\n\n[dependencies]\nserde = { version = "1", features = [] }\n',
      marketplace: {
        name: 'volute',
        description: 'Claude Code plugins from the fixture project.',
        owner: { name: 'Volute', url: 'https://example.test/volute' },
        plugins: [{ name: 'cvc', source: { source: 'git-subdir', url: REPOSITORY_URL, path: PLUGIN_PATH, ref: 'v1.2.3' }, ...structuredClone(display) }],
      },
      plugin: { name: 'cvc', version: '1.2.3', ...structuredClone(display) },
      hooks: {
        description: 'Captures sessions.',
        hooks: {
          SessionStart: [{ matcher: 'startup|resume', hooks: [{ ...SESSION_START_HOOK }] }],
          PostToolUse: [{ hooks: [{ ...INGEST_HOOK }] }],
          Stop: [{ hooks: [{ ...INGEST_HOOK }] }],
          SessionEnd: [{ hooks: [{ ...INGEST_HOOK }] }],
        },
      },
      extraFiles: { [scriptFile]: checkScript, [wrapperFile]: wrapperScript },
    };
  };
  const write = (dir, path, content) => {
    mkdirSync(dirname(join(dir, path)), { recursive: true });
    writeFileSync(join(dir, path), typeof content === 'string' ? content : `${JSON.stringify(content, null, 2)}\n`);
  };
  // A null section leaves its file out, and a string is written verbatim (to test bad JSON).
  const build = (dir, tree) => {
    const files = [
      [CARGO_FILE, tree.cargo], [MARKETPLACE_FILE, tree.marketplace],
      [`${PLUGIN_PATH}/.claude-plugin/plugin.json`, tree.plugin], [`${PLUGIN_PATH}/hooks/hooks.json`, tree.hooks],
      ...Object.entries(tree.extraFiles ?? {}),
    ];
    for (const [path, content] of files) if (content !== null) write(dir, path, content);
  };
  const posixShell = process.platform !== 'win32' && existsSync('/bin/sh');

  const cases = [
    ['marketplace file missing', t => { t.marketplace = null; }, /marketplace\.json: file is missing/],
    ['marketplace not JSON', t => { t.marketplace = '{ nope'; }, /marketplace\.json: is not valid JSON/],
    ['reserved marketplace name', t => { t.marketplace.name = 'github'; }, /reserved by Claude Code/],
    ['reserved marketplace prefix', t => { t.marketplace.name = 'claudeai-tools'; }, /reserved by Claude Code/],
    ['marketplace name syntax', t => { t.marketplace.name = '-volute'; }, /may contain only letters/],
    ['marketplace renamed', t => { t.marketplace.name = 'volute-tools'; }, /must be "volute"/],
    ['marketplace description missing', t => { delete t.marketplace.description; }, /marketplace\.json: `description` must be a non-empty string/],
    ['marketplace description blank', t => { t.marketplace.description = '  '; }, /marketplace\.json: `description` must be a non-empty string/],
    ['owner missing', t => { delete t.marketplace.owner; }, /`owner` must be an object/],
    ['owner without name', t => { t.marketplace.owner = { url: 'https://example.test' }; }, /owner\.name/],
    ['two plugin entries', t => { t.marketplace.plugins.push({ ...t.marketplace.plugins[0] }); }, /exactly one plugin/],
    ['wrong entry name', t => { t.marketplace.plugins[0].name = 'other'; }, /plugins\[0\]\.name` must be "cvc"/],
    ['entry version', t => { t.marketplace.plugins[0].version = '1.2.3'; }, /must not set `version`/],
    ...['hooks', 'mcpServers', 'commands', 'agents', 'skills', 'lspServers', 'outputStyles', 'strict', 'dependencies'].map(key => [`entry ${key}`, t => { t.marketplace.plugins[0][key] = key === 'strict' ? false : {}; }, new RegExp(`plugins\\[0\\]\` has unexpected key "${key}"; only .* are allowed: an entry can carry plugin-manifest fields`)]),
    ...['renames', 'forceRemoveDeletedPlugins', 'metadata', '$schema'].map(key => [`marketplace ${key}`, t => { t.marketplace[key] = key === '$schema' ? 'https://example.test/schema.json' : {}; }, new RegExp(`the marketplace has unexpected key "${key.replace('$', '\\$')}"; only "name", "description", "owner", "plugins" are allowed`)]),
    ['owner email', t => { t.marketplace.owner.email = 'someone@example.test'; }, /`owner` has unexpected key "email"; only "name", "url" are allowed/],
    ['owner another key', t => { t.marketplace.owner.role = 'maintainer'; }, /`owner` has unexpected key "role"/],
    ['relative source', t => { t.marketplace.plugins[0].source = './plugins/claude-code'; }, /must be a git-subdir object pinned to a release tag, not "\.\/plugins\/claude-code"/],
    ['github shorthand source', t => { t.marketplace.plugins[0].source = { source: 'github', repo: 'meirka8/volute' }; }, /source`\.source must be "git-subdir"/],
    ['source kind changed', t => { t.marketplace.plugins[0].source.source = 'github'; }, /source`\.source must be "git-subdir"; received "github"/],
    ['url as owner/repo', t => { t.marketplace.plugins[0].source.url = 'meirka8/volute'; }, /source`\.url must be exactly "https:\/\/github\.com\/meirka8\/volute\.git" \(a full https URL, not owner\/repo\); received "meirka8\/volute"/],
    ['url without .git', t => { t.marketplace.plugins[0].source.url = 'https://github.com/meirka8/volute'; }, /source`\.url must be exactly/],
    ['path changed', t => { t.marketplace.plugins[0].source.path = 'plugins/other'; }, /source`\.path must be exactly "plugins\/claude-code"/],
    ['path missing', t => { delete t.marketplace.plugins[0].source.path; }, /source`\.path must be exactly/],
    ['ref differs from v + version', t => { t.marketplace.plugins[0].source.ref = 'v1.2.4'; }, /source\.ref` must be "v1\.2\.3" \(v \+ the plugin manifest version in plugins\/claude-code\/\.claude-plugin\/plugin\.json\); received "v1\.2\.4"/],
    ['ref without v', t => { t.marketplace.plugins[0].source.ref = '1.2.3'; }, /source`\.ref must match/],
    ['ref a branch', t => { t.marketplace.plugins[0].source.ref = 'main'; }, /source`\.ref must match .*never a branch/],
    ['ref missing', t => { delete t.marketplace.plugins[0].source.ref; }, /source`\.ref must match/],
    ['ref a prerelease', t => { t.marketplace.plugins[0].source.ref = 'v1.2.3-rc.1'; }, /source`\.ref must match/],
    ['source with sha', t => { t.marketplace.plugins[0].source.sha = 'a'.repeat(40); }, /must not set `sha`/],
    ['source with another key', t => { t.marketplace.plugins[0].source.branch = 'main'; }, /unexpected key "branch"/],
    ['plugin directory missing', t => { t.plugin = null; t.hooks = null; t.extraFiles = {}; }, /source`\.path "plugins\/claude-code" does not exist as a directory/],
    ...DISPLAY_FIELDS.map(field => [`entry ${field} drifted`, t => { t.marketplace.plugins[0][field] = field === 'keywords' ? ['other'] : field === 'author' ? { name: 'Someone' } : 'drifted'; }, new RegExp(`plugins\\[0\\]\\.${field}\` must equal \`${field}\` in plugins/claude-code/\\.claude-plugin/plugin\\.json`)]),
    ...DISPLAY_FIELDS.map(field => [`entry ${field} missing`, t => { delete t.marketplace.plugins[0][field]; }, new RegExp(`plugins\\[0\\]\\.${field}\` must equal \`${field}\``)]),
    ['plugin manifest missing', t => { t.plugin = null; }, /plugin\.json: file is missing/],
    ['plugin name differs from entry', t => { t.plugin.name = 'cvc2'; }, /must equal the marketplace entry name/],
    ['plugin version differs from the CLI', t => { t.plugin.version = '1.2.4'; }, /does not match cvc-cli\/Cargo\.toml version/],
    ['plugin version not SemVer', t => { t.plugin.version = '1.2'; }, /stable MAJOR\.MINOR\.PATCH/],
    ['plugin prerelease version', t => { t.plugin.version = '1.2.3-rc.1'; }, /stable MAJOR\.MINOR\.PATCH/],
    ['plugin licence differs', t => { t.plugin.license = 'MIT'; t.marketplace.plugins[0].license = 'MIT'; }, /license` "MIT" does not match/],
    ['plugin licence missing', t => { delete t.plugin.license; delete t.marketplace.plugins[0].license; }, /`license` must be a non-empty/],
    ['plugin declares hooks', t => { t.plugin.hooks = './hooks/hooks.json'; }, /`hooks` must not be declared/],
    ['hooks file missing', t => { t.hooks = null; }, /hooks\.json: file is missing/],
    ['hooks file not JSON', t => { t.hooks = '{"hooks": '; }, /hooks\.json: is not valid JSON/],
    ['hooks key not an object', t => { t.hooks.hooks = []; }, /`hooks` must be an object/],
    ['hooks description missing', t => { delete t.hooks.description; }, /`description` must be a non-empty/],
    ['unexpected top-level key', t => { t.hooks.extra = true; }, /unexpected top-level key "extra"/],
    ['unexpected event', t => { t.hooks.hooks.UserPromptSubmit = [{ hooks: [{ ...INGEST_HOOK }] }]; }, /unexpected hook event "UserPromptSubmit"/],
    ['PostToolUse missing', t => { delete t.hooks.hooks.PostToolUse; }, /hooks\.PostToolUse must be an array of exactly one/],
    ['ingest command without the wrapper', t => { t.hooks.hooks.Stop[0].hooks[0].command = 'cvc ingest claude-code --hook'; }, /hooks\.Stop\[0\]\.hooks\[0\] must be exactly/],
    ['ingest command changed', t => { t.hooks.hooks.Stop[0].hooks[0].command = INGEST_HOOK.command.replace(' --hook', ''); }, /hooks\.Stop\[0\]\.hooks\[0\] must be exactly/],
    ['ingest command unquoted', t => { t.hooks.hooks.PostToolUse[0].hooks[0].command = INGEST_HOOK.command.replace(`"\${CLAUDE_PLUGIN_ROOT}/${WRAPPER_SCRIPT}"`, `\${CLAUDE_PLUGIN_ROOT}/${WRAPPER_SCRIPT}`); }, /hooks\.PostToolUse\[0\]\.hooks\[0\] must be exactly/],
    ['ingest command with sh instead of /bin/sh', t => { t.hooks.hooks.Stop[0].hooks[0].command = INGEST_HOOK.command.replace('/bin/sh', 'sh'); }, /hooks\.Stop\[0\]\.hooks\[0\] must be exactly/],
    ['ingest command without the exit 2 guard', t => { t.hooks.hooks.SessionEnd[0].hooks[0].command = INGEST_HOOK.command.split(';')[0]; }, /hooks\.SessionEnd\[0\]\.hooks\[0\] must be exactly/],
    ['ingest command mapping 2 to 0', t => { t.hooks.hooks.PostToolUse[0].hooks[0].command = INGEST_HOOK.command.replace('s=1', 's=0'); }, /hooks\.PostToolUse\[0\]\.hooks\[0\] must be exactly/],
    ['ingest timeout changed', t => { t.hooks.hooks.SessionEnd[0].hooks[0].timeout = 5; }, /hooks\.SessionEnd\[0\]\.hooks\[0\] must be exactly/],
    ['ingest hook made async', t => { t.hooks.hooks.PostToolUse[0].hooks[0].async = true; }, /hooks\.PostToolUse\[0\]\.hooks\[0\] must be exactly/],
    ['ingest event with matcher', t => { t.hooks.hooks.PostToolUse[0].matcher = 'Bash'; }, /hooks\.PostToolUse\[0\] has unexpected key "matcher"/],
    ['ingest event with two groups', t => { t.hooks.hooks.SessionEnd.push({ hooks: [{ ...INGEST_HOOK }] }); }, /hooks\.SessionEnd must be an array of exactly one/],
    ['ingest group with two hooks', t => { t.hooks.hooks.Stop[0].hooks.push({ ...INGEST_HOOK }); }, /hooks\.Stop\[0\]\.hooks must be an array of exactly one/],
    ['wrapper script missing', t => { t.extraFiles[wrapperFile] = null; }, /hooks\.PostToolUse references "\$\{CLAUDE_PLUGIN_ROOT\}\/scripts\/run-cvc\.sh", but plugins\/claude-code\/scripts\/run-cvc\.sh does not exist/],
    ['SessionStart without matcher', t => { delete t.hooks.hooks.SessionStart[0].matcher; }, /SessionStart\[0\]\.matcher must be "startup\|resume"/],
    ['SessionStart wrong timeout', t => { t.hooks.hooks.SessionStart[0].hooks[0].timeout = 10; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart command unquoted', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = '/bin/sh ${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh'; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart command with sh instead of /bin/sh', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = 'sh "${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh"'; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart command changed', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = 'true'; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart runs ingest', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = `${SESSION_START_HOOK.command} && cvc ingest claude-code --hook`; }, /must only check the CLI and must not run `ingest`/],
    ['SessionStart script missing', t => { t.extraFiles[scriptFile] = null; }, /hooks\.SessionStart references "\$\{CLAUDE_PLUGIN_ROOT\}\/scripts\/check-cvc\.sh", but plugins\/claude-code\/scripts\/check-cvc\.sh does not exist/],
    ['SessionStart script renamed', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = '/bin/sh "${CLAUDE_PLUGIN_ROOT}/scripts/other.sh"'; }, /scripts\/other\.sh does not exist/],
    ['script outside the plugin', t => { t.hooks.hooks.Stop[0].hooks[0].command = 'sh ${CLAUDE_PLUGIN_ROOT}/../outside.sh'; }, /outside the plugin directory/],
    ['script missing', t => { t.hooks.hooks.SessionEnd[0].hooks[0].command = '"${CLAUDE_PLUGIN_ROOT}/scripts/ingest.sh"'; }, /scripts\/ingest\.sh does not exist/],
  ];
  if (posixShell) {
    // These run the scripts, so each one breaks a fixture script in a single way.
    const script = (name, content, expected) => cases.push([name, t => { t.extraFiles[scriptFile] = content; }, expected]);
    const wrapper = (name, content, expected) => cases.push([name, t => { t.extraFiles[wrapperFile] = content; }, expected]);
    const noCvc = /with no cvc on PATH must/;
    const unrunnable = code => new RegExp(`with a cvc that exits ${code} when run must print a \`systemMessage\` that mentions "could not be run"`);
    const locate = 'if [ "$CVC_PLUGIN_LOCATE_ONLY" = 1 ]; then\n  printf \'%s\\n\' "$found"\n  exit 0\nfi\n';

    script('script silent without cvc', '#!/bin/sh\nexit 0\n', /with no cvc on PATH must print exactly one JSON object/);
    script('script ignores PATH without cvc', edited('is not on the PATH Claude Code runs hooks with', 'is missing'), /with no cvc on PATH must print a `systemMessage` that mentions PATH/);
    script('script noisy when the probe passes', edited('    0) exit 0 ;;', '    0) message="CVC: all good" ;;'), /accepts the probe must print nothing/);
    script('script silent when the probe fails', checkScript.split('\n').map(line => (line.includes('older than') ? '    1) message= ;;' : line)).join('\n'), /fails the probe must print exactly one JSON object/);
    script('script omits older', edited('is older than', 'differs from'), /fails the probe must print a `systemMessage` that mentions "older"/);
    script('script omits relative', edited('reached only through a relative PATH entry', 'in an odd place'), /with a cvc reachable only through a relative PATH entry must print a `systemMessage` that mentions "relative"/);
    script('script silent for a relative-only cvc', edited('    message="CVC: the cvc on PATH is reached only through a relative PATH entry, which the cvc plugin does not use."', '    exit 0'), /with a cvc reachable only through a relative PATH entry must print exactly one JSON object/);
    script('script reports a 127 cvc as older', edited('    1) message=', '    1|127) message='), unrunnable(127));
    script('script reports every other failure as older', edited('    *) message="CVC: the cvc found on the PATH Claude Code runs hooks with could not be run, so the plugin cannot capture this session."', '    *) message="CVC: the cvc on PATH is older than the plugin needs."'), unrunnable(126));
    script('script blames a relative PATH entry for an unrunnable cvc', edited('could not be run, so', 'could not be run, as with a relative PATH entry, so'), /with a cvc that exits 127 when run must not blame a relative PATH entry/);
    script('script reports an unrunnable cvc as missing', edited('the cvc found on the PATH Claude Code runs hooks with could not be run', 'cvc is not on the PATH'), unrunnable(127));
    script('script fails under a path with a space', edited('#!/bin/sh\n', '#!/bin/sh\ncase "$0" in *" "*) exit 3 ;; esac\n'), /containing a space, with a cvc that fails the probe must exit 0; exited 3/);
    script('script exits non-zero', edited('"$message"\nexit 0\n', '"$message"\nexit 1\n'), /must exit 0; exited 1/);
    script('script writes to stderr', edited('"$message"\nexit 0\n', '"$message"\nprintf \'oops\\n\' >&2\nexit 0\n'), /must write nothing to stderr/);
    script('script probes with other arguments', edited(INGEST_ARGUMENTS, 'ingest claude-code'), /must probe cvc exactly once with `ingest claude-code --hook`; the stub saw "ingest claude-code\\n"/);
    script('script probe keeps the session project directory', edited('CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe "$cvc"', '"$cvc"'), /must run the probe with CLAUDE_PROJECT_DIR=\/nonexistent\/cvc-plugin-probe, so it does not depend on the session's project; the stub saw "CLAUDE_PROJECT_DIR=[^"]*cvc-hook-[^"]*project\\n"/);
    script('script probe overrides with another directory', edited('CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe "$cvc"', 'CLAUDE_PROJECT_DIR=/tmp "$cvc"'), /must run the probe with CLAUDE_PROJECT_DIR=\/nonexistent\/cvc-plugin-probe.*the stub saw "CLAUDE_PROJECT_DIR=\/tmp\\n"/);
    script('script probes twice', edited('  status=$?\n', `  printf '%s' x | "$cvc" ${INGEST_ARGUMENTS} >/dev/null 2>&1\n  status=$?\n`), /must probe cvc exactly once/);
    script('script never probes', '#!/bin/sh\ncommand -v cvc >/dev/null 2>&1 || printf \'%s\\n\' \'{"systemMessage":"PATH relative"}\'\nexit 0\n', /must probe cvc exactly once with `ingest claude-code --hook`; the stub saw ""/);
    script('script locates cvc without run-cvc.sh', edited('cvc=$(CVC_PLUGIN_LOCATE_ONLY=1 /bin/sh "$here/run-cvc.sh" 2>/dev/null)', 'cvc=$(command -v cvc)'), /must not run a cvc found through a relative PATH entry; the cvc in the current directory ran/);
    script('script calls sh by name', edited('/bin/sh "$here/run-cvc.sh"', 'sh "$here/run-cvc.sh"'), /must not run a sh found through a relative PATH entry; the sh in the current directory ran/);
    script('script falls back to . without a directory of its own', edited('  *) exit 0 ;;', '  *) here=. ;;'), /SessionStart script run by a bare file name must do nothing/);

    wrapper('wrapper honours relative PATH entries', wrapperEdited('  case $dir in\n    /*) ;;\n    *) continue ;;\n  esac\n', ''), /must not run a cvc found through a relative PATH entry; the cvc in the current directory ran/);
    wrapper('wrapper stops at a relative PATH entry', wrapperEdited('*) continue ;;', '*) break ;;'), /with relative PATH entries, a cvc in the current directory and an absolute cvc that accepts the probe must print nothing/);
    wrapper('wrapper expands pattern characters', wrapperEdited('set -f\n', ''), /with a PATH entry containing a glob character must exit 127; exited 0/);
    wrapper('wrapper prints another message', wrapperEdited('cvc: command not found', 'cvc: not found'), /with only a cvc in a relative PATH entry must print "cvc: command not found in an absolute PATH directory/);
    wrapper('wrapper exits with another status', wrapperEdited('exit 127', 'exit 1'), /with only a cvc in a relative PATH entry must exit 127; exited 1/);
    wrapper('wrapper drops standard input', wrapperEdited('exec "$found" "$@"', 'exec "$found" "$@" </dev/null'), /must pass its standard input to cvc unchanged/);
    wrapper('wrapper drops arguments', wrapperEdited('exec "$found" "$@"', 'exec "$found"'), /ingest hook command with an absolute cvc must run cvc exactly once with `ingest claude-code --hook`; the stub saw "\\n"/);
    wrapper('wrapper drops CLAUDE_PROJECT_DIR', wrapperEdited('exec "$found" "$@"', 'unset CLAUDE_PROJECT_DIR\nexec "$found" "$@"'), /must pass CLAUDE_PROJECT_DIR through to cvc/);
    wrapper('wrapper has no locate mode', wrapperEdited(locate, ''), /run-cvc\.sh in locate mode .* must print the absolute path of the cvc and a newline/);
    wrapper('wrapper runs the cvc in locate mode', wrapperEdited('  printf \'%s\\n\' "$found"\n  exit 0\n', '  exec "$found" "$@"\n'), /run-cvc\.sh in locate mode .* must not run the cvc; the stub saw/);
    wrapper('wrapper prints the path without a newline', wrapperEdited('  printf \'%s\\n\' "$found"\n  exit 0\n', '  printf \'%s\' "$found"\n  exit 0\n'), /must print the absolute path of the cvc and a newline/);
    wrapper('wrapper locates nothing in locate mode', wrapperEdited('exit 127', 'exit 0'), /run-cvc\.sh in locate mode with only a cvc in a relative PATH entry must exit 127; exited 0/);
  }

  const base = mkdtempSync(join(tmpdir(), 'check-claude-plugin-'));
  try {
    const attempt = (name, mutate) => {
      const tree = baseline();
      mutate(tree);
      const dir = join(base, name.replace(/[^A-Za-z0-9]+/g, '-'));
      build(dir, tree);
      return validate(dir);
    };
    const clean = attempt('baseline', () => {});
    assert(clean.length === 0, `the baseline fixture must pass; got ${show(clean)}`);
    const withScript = attempt('shipped-script', t => {
      t.hooks.hooks.PostToolUse[0].hooks[0] = { type: 'command', command: '"${CLAUDE_PLUGIN_ROOT}/scripts/ingest.sh"', timeout: 30 };
      t.extraFiles[`${PLUGIN_PATH}/scripts/ingest.sh`] = '#!/bin/sh\n';
    });
    // The script reference resolves, but this hook is no longer the exact ingest command.
    assert(withScript.length === 1 && /hooks\.PostToolUse\[0\]\.hooks\[0\] must be exactly/.test(withScript[0].message), `a shipped script must resolve; got ${show(withScript)}`);
    for (const [name, mutate, expected] of cases) {
      const found = attempt(name, mutate);
      assert(found.some(problem => expected.test(`${problem.file}: ${problem.message}`)), `${name}: expected ${expected} but got ${show(found)}`);
    }
    // A failing run must name a file and exit non-zero; check the real entry point does.
    const failing = join(base, 'cli-failing');
    const tree = baseline();
    tree.plugin.version = '9.9.9';
    build(failing, tree);
    let status = 0;
    try { execFileSync(process.execPath, [fileURLToPath(import.meta.url), '--root', failing], { stdio: 'pipe' }); } catch (error) { status = error.status; }
    assert(status === 1, `a failing tree must exit 1; exited ${status}`);
    console.log(`Claude Code plugin checker self-tests passed (${cases.length + 3} scenarios${posixShell ? '' : '; shell behaviour skipped, no POSIX sh'}).`);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
}

const args = process.argv.slice(2);
const usage = 'Usage: check-claude-plugin.mjs [--self-test | --root <dir>]';
if (args.length === 1 && args[0] === '--self-test') {
  selfTest();
} else if (args.length === 0 || (args.length === 2 && args[0] === '--root')) {
  const root = args.length === 2 ? resolve(args[1]) : resolve(dirname(fileURLToPath(import.meta.url)), '../..');
  const problems = validate(root);
  if (problems.length > 0) {
    report(problems);
    console.error(`Claude Code plugin check failed with ${problems.length} problem${problems.length === 1 ? '' : 's'}.`);
    process.exit(1);
  }
  console.log('Claude Code plugin check passed: marketplace "volute", plugin "cvc", and hooks match the documented contract.');
} else {
  throw new Error(usage);
}
