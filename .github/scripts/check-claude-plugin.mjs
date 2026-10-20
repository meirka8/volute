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
 * Releases are gated separately: check-release-version.mjs requires the plugin
 * manifest version to equal the release tag. This script only requires it to
 * equal cvc-cli's version, so a pull request cannot drift from the CLI it needs.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';

const MARKETPLACE_NAME = 'volute';
const PLUGIN_NAME = 'cvc';
const MARKETPLACE_FILE = '.claude-plugin/marketplace.json';
const CARGO_FILE = 'cvc-cli/Cargo.toml';
// Claude Code rejects these marketplace names, and anything starting `claudeai-`.
const RESERVED_NAMES = new Set([
  'claude-code-marketplace', 'anthropic-plugins', 'claude-community', 'claude-plugins-official',
  'healthcare', 'anthropic-plugin-directory', 'npm', 'pip', 'uv', 'cargo', 'github', 'gh',
]);
const NAME_SYNTAX = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
// Same stable-only SemVer the release version gate accepts.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const INGEST_HOOK = Object.freeze({ type: 'command', command: 'cvc ingest claude-code --hook', timeout: 30 });
const INGEST_EVENTS = ['PostToolUse', 'Stop', 'SessionEnd'];
const SESSION_START_MATCHER = 'startup|resume';
// SessionStart only checks that the CLI is present and new enough; it never ingests.
const SESSION_START_SCRIPT = 'scripts/check-cvc.sh';
const SESSION_START_HOOK = Object.freeze({ type: 'command', command: `sh "\${CLAUDE_PLUGIN_ROOT}/${SESSION_START_SCRIPT}"`, timeout: 30 });

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

  function checkName(file, label, name) {
    if (!nonEmpty(name)) { fail(file, `${label} must be a non-empty string.`); return false; }
    if (!NAME_SYNTAX.test(name)) { fail(file, `${label} ${show(name)} may contain only letters, digits, ".", "_" and "-", and must start with a letter or digit.`); return false; }
    return true;
  }

  // Returns { pluginDir, entryName } when the marketplace is usable enough to continue.
  function checkMarketplace() {
    const path = join(root, MARKETPLACE_FILE);
    const marketplace = readJson(path);
    if (marketplace === undefined) return undefined;
    if (!isObject(marketplace)) { fail(MARKETPLACE_FILE, 'must be a JSON object.'); return undefined; }

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
      for (const key of ['email', 'url']) if (key in marketplace.owner && !nonEmpty(marketplace.owner[key])) fail(MARKETPLACE_FILE, `\`owner.${key}\` must be a non-empty string when present.`);
    }

    if (!Array.isArray(marketplace.plugins) || marketplace.plugins.length !== 1) {
      fail(MARKETPLACE_FILE, `\`plugins\` must list exactly one plugin (${show(PLUGIN_NAME)}); received ${Array.isArray(marketplace.plugins) ? marketplace.plugins.length : typeof marketplace.plugins}.`);
      return undefined;
    }
    const entry = marketplace.plugins[0];
    if (!isObject(entry)) { fail(MARKETPLACE_FILE, '`plugins[0]` must be an object.'); return undefined; }
    if (checkName(MARKETPLACE_FILE, '`plugins[0].name`', entry.name) && entry.name !== PLUGIN_NAME) fail(MARKETPLACE_FILE, `\`plugins[0].name\` must be ${show(PLUGIN_NAME)}; received ${show(entry.name)}.`);
    if (!nonEmpty(entry.description)) fail(MARKETPLACE_FILE, '`plugins[0].description` must be a non-empty string.');
    // The manifest version wins over a marketplace version, so a second copy can only go stale.
    if ('version' in entry) fail(MARKETPLACE_FILE, '`plugins[0]` must not set `version`; the plugin manifest is the single place the version lives.');

    if (typeof entry.source !== 'string' || !entry.source.startsWith('./')) { fail(MARKETPLACE_FILE, `\`plugins[0].source\` must be a relative path string starting with "./"; received ${show(entry.source)}.`); return undefined; }
    if (entry.source.split(/[\\/]/).includes('..')) { fail(MARKETPLACE_FILE, `\`plugins[0].source\` must not contain "..": ${show(entry.source)}.`); return undefined; }
    const pluginDir = resolve(root, entry.source);
    if (pluginDir !== root && !pluginDir.startsWith(root + sep)) { fail(MARKETPLACE_FILE, `\`plugins[0].source\` ${show(entry.source)} resolves outside the repository.`); return undefined; }
    if (!existsSync(pluginDir) || !statSync(pluginDir).isDirectory()) { fail(MARKETPLACE_FILE, `\`plugins[0].source\` ${show(entry.source)} does not exist as a directory.`); return undefined; }
    return { pluginDir, entryName: entry.name };
  }

  function checkManifest(pluginDir, entryName, cargo) {
    const path = join(pluginDir, '.claude-plugin', 'plugin.json');
    const file = rel(path);
    const manifest = readJson(path);
    if (manifest === undefined) return;
    if (!isObject(manifest)) { fail(file, 'must be a JSON object.'); return; }

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
  }

  // Runs the SessionStart command the way Claude Code would, with a PATH that holds only `sh`
  // and, when asked, a stub `cvc`, and with CLAUDE_PROJECT_DIR set to the session's (real)
  // project directory, which Claude Code gives SessionStart hooks. The stub logs its arguments
  // and the CLAUDE_PROJECT_DIR it was started with, so the probe can be verified.
  function checkSessionStartBehaviour(file, pluginDir, command) {
    if (process.platform === 'win32' || !existsSync('/bin/sh')) return; // POSIX sh is needed to run it
    const base = mkdtempSync(join(tmpdir(), 'cvc-hook-'));
    const expectedArgv = 'ingest claude-code --hook\n';
    // Hook mode routes by CLAUDE_PROJECT_DIR, so the probe must override the session's value.
    const probeProjectDir = '/nonexistent/cvc-plugin-probe';
    const expectedProjectDir = `CLAUDE_PROJECT_DIR=${probeProjectDir}\n`;
    const projectDir = join(base, 'project');
    // The stub prints the variable it was started with (a literal shell word, expanded by the stub).
    const stubEnvLine = '"CLAUDE_PROJECT_DIR=${CLAUDE_PROJECT_DIR-UNSET}"';
    mkdirSync(projectDir, { recursive: true });
    // Returns { stdout, argv, projectDir } after checking the exit status and stderr, or undefined on failure.
    function run(name, label, stubExit, root = pluginDir) {
      const bin = join(base, name, 'bin');
      const log = join(base, name, 'argv.log');
      const envLog = join(base, name, 'env.log');
      mkdirSync(bin, { recursive: true });
      symlinkSync('/bin/sh', join(bin, 'sh'));
      if (stubExit !== undefined) writeFileSync(join(bin, 'cvc'), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nprintf '%s\\n' ${stubEnvLine} >> '${envLog}'\nIFS= read -r _line\nexit ${stubExit}\n`, { mode: 0o755 });
      const result = spawnSync('/bin/sh', ['-c', command], { env: { PATH: bin, CLAUDE_PLUGIN_ROOT: root, CLAUDE_PROJECT_DIR: projectDir }, encoding: 'utf8', timeout: 15_000 });
      if (result.error) { fail(file, `SessionStart command ${label} could not run: ${result.error.message}`); return undefined; }
      if (result.status !== 0) { fail(file, `SessionStart command ${label} must exit 0; exited ${result.status}${result.stderr ? `: ${result.stderr.trim()}` : ''}.`); return undefined; }
      if (result.stderr !== '') { fail(file, `SessionStart command ${label} must write nothing to stderr; wrote ${show(result.stderr)}.`); return undefined; }
      return { stdout: result.stdout, argv: existsSync(log) ? readFileSync(log, 'utf8') : '', projectDir: existsSync(envLog) ? readFileSync(envLog, 'utf8') : '' };
    }
    // The probe must run cvc once, with the hook-mode arguments, and with the probe's own project directory.
    function checkProbe(who, result) {
      if (result.argv !== expectedArgv) fail(file, `${who} must probe cvc exactly once with \`${expectedArgv.trim()}\`; the stub saw ${show(result.argv)}.`);
      else if (result.projectDir !== expectedProjectDir) fail(file, `${who} must run the probe with ${expectedProjectDir.trim()}, so it does not depend on the session's project; the stub saw ${show(result.projectDir)}.`);
    }
    // One JSON line whose `systemMessage` matches; the exact wording is pinned by the Rust tests.
    function message(label, stdout, pattern, mention) {
      const lines = stdout.split('\n').filter(line => line.trim() !== '');
      let parsed;
      try { parsed = lines.length === 1 ? JSON.parse(lines[0]) : undefined; } catch { parsed = undefined; }
      if (!isObject(parsed) || !nonEmpty(parsed.systemMessage)) fail(file, `SessionStart command ${label} must print exactly one JSON object with a \`systemMessage\`; printed ${show(stdout)}.`);
      else if (!pattern.test(parsed.systemMessage)) fail(file, `SessionStart command ${label} must print a \`systemMessage\` that mentions ${mention}; printed ${show(parsed.systemMessage)}.`);
    }
    try {
      const missing = run('missing', 'with no cvc on PATH', undefined);
      if (missing) message('with no cvc on PATH', missing.stdout, /PATH/, 'PATH');

      const current = run('current', 'with a cvc that accepts the probe', 0);
      if (current) {
        if (current.stdout !== '') fail(file, `SessionStart command with a cvc that accepts the probe must print nothing; printed ${show(current.stdout)}.`);
        checkProbe('SessionStart command', current);
      }

      const old = run('old', 'with a cvc that fails the probe', 1);
      if (old) {
        message('with a cvc that fails the probe', old.stdout, /older/, '"older"');
        checkProbe('SessionStart command', old);
      }

      // Claude Code keeps installed plugins under a cache directory that can contain a space
      // (a home directory such as "Jane Doe"), and the command expands CLAUDE_PLUGIN_ROOT in the
      // shell. Run the same check from a copy of the plugin at such a path.
      const spaced = join(base, 'plugin root');
      cpSync(pluginDir, spaced, { recursive: true });
      const spacedLabel = 'in a CLAUDE_PLUGIN_ROOT containing a space, with a cvc that fails the probe';
      const inSpace = run('spaced', spacedLabel, 1, spaced);
      if (inSpace) {
        message(spacedLabel, inSpace.stdout, /older/, '"older"');
        checkProbe(`SessionStart command ${spacedLabel}`, inSpace);
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

    for (const event of INGEST_EVENTS) {
      const found = onlyHook(event, ['hooks']);
      if (found && !isDeepStrictEqual(found.hook, INGEST_HOOK)) fail(file, `hooks.${event}[0].hooks[0] must be exactly ${show(INGEST_HOOK)}; received ${show(found.hook)}.`);
    }

    const start = onlyHook('SessionStart', ['matcher', 'hooks']);
    if (start) {
      const { group, hook } = start;
      if (group.matcher !== SESSION_START_MATCHER) fail(file, `hooks.SessionStart[0].matcher must be ${show(SESSION_START_MATCHER)}; received ${show(group.matcher)}.`);
      if (!isDeepStrictEqual(hook, SESSION_START_HOOK)) fail(file, `hooks.SessionStart[0].hooks[0] must be exactly ${show(SESSION_START_HOOK)}; received ${show(hook)}.`);
      if (typeof hook.command === 'string') {
        if (hook.command.includes('ingest')) fail(file, 'hooks.SessionStart command must only check the CLI and must not run `ingest`.');
        // Run it only when it is the expected command and its script ships, so one mistake is not reported twice.
        else if (isDeepStrictEqual(hook, SESSION_START_HOOK) && existsSync(join(pluginDir, SESSION_START_SCRIPT))) checkSessionStartBehaviour(file, pluginDir, hook.command);
      }
    }

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
    checkManifest(located.pluginDir, located.entryName, cargo);
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
  const scriptFile = `plugins/claude-code/${SESSION_START_SCRIPT}`;
  // A SessionStart script that meets the contract, using shell builtins only.
  const checkScript = String.raw`#!/bin/sh
if ! command -v cvc >/dev/null 2>&1; then
  printf '%s\n' '{"systemMessage":"CVC: cvc is not on the PATH Claude Code runs hooks with."}'
  exit 0
fi
if ! printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe cvc ingest claude-code --hook >/dev/null 2>&1; then
  printf '%s\n' '{"systemMessage":"CVC: the cvc on PATH is older than the plugin needs."}'
fi
exit 0
`;
  const edited = (from, to) => {
    assert(checkScript.includes(from), `fixture script must contain ${show(from)}`);
    return checkScript.replace(from, to);
  };
  const baseline = () => ({
    cargo: '[package]\nname = "cvc-cli"\nversion = "1.2.3"\nlicense = "Apache-2.0"\n\n[dependencies]\nserde = { version = "1", features = [] }\n',
    marketplace: {
      name: 'volute',
      description: 'Claude Code plugins from the fixture project.',
      owner: { name: 'Volute', url: 'https://example.test/volute' },
      plugins: [{ name: 'cvc', source: './plugins/claude-code', description: 'Captures sessions.' }],
    },
    plugin: {
      name: 'cvc', version: '1.2.3', description: 'Captures sessions.', author: { name: 'Volute' },
      homepage: 'https://example.test/volute', repository: 'https://example.test/volute', license: 'Apache-2.0', keywords: ['cvc'],
    },
    hooks: {
      description: 'Captures sessions.',
      hooks: {
        SessionStart: [{ matcher: 'startup|resume', hooks: [{ ...SESSION_START_HOOK }] }],
        PostToolUse: [{ hooks: [{ ...INGEST_HOOK }] }],
        Stop: [{ hooks: [{ ...INGEST_HOOK }] }],
        SessionEnd: [{ hooks: [{ ...INGEST_HOOK }] }],
      },
    },
    extraFiles: { [scriptFile]: checkScript },
  });
  const write = (dir, path, content) => {
    mkdirSync(dirname(join(dir, path)), { recursive: true });
    writeFileSync(join(dir, path), typeof content === 'string' ? content : `${JSON.stringify(content, null, 2)}\n`);
  };
  // A null section leaves its file out, and a string is written verbatim (to test bad JSON).
  const build = (dir, tree) => {
    const files = [
      [CARGO_FILE, tree.cargo], [MARKETPLACE_FILE, tree.marketplace],
      ['plugins/claude-code/.claude-plugin/plugin.json', tree.plugin], ['plugins/claude-code/hooks/hooks.json', tree.hooks],
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
    ['entry without description', t => { delete t.marketplace.plugins[0].description; }, /plugins\[0\]\.description/],
    ['entry version', t => { t.marketplace.plugins[0].version = '1.2.3'; }, /must not set `version`/],
    ['absolute source', t => { t.marketplace.plugins[0].source = '/plugins/claude-code'; }, /relative path string/],
    ['source object', t => { t.marketplace.plugins[0].source = { source: 'github', repo: 'a/b' }; }, /relative path string/],
    ['source with ..', t => { t.marketplace.plugins[0].source = './plugins/../plugins/claude-code'; }, /must not contain "\.\."/],
    ['source directory missing', t => { t.marketplace.plugins[0].source = './plugins/elsewhere'; }, /does not exist as a directory/],
    ['plugin manifest missing', t => { t.plugin = null; }, /plugin\.json: file is missing/],
    ['plugin name differs from entry', t => { t.plugin.name = 'cvc2'; }, /must equal the marketplace entry name/],
    ['plugin version differs from the CLI', t => { t.plugin.version = '1.2.4'; }, /does not match cvc-cli\/Cargo\.toml version/],
    ['plugin version not SemVer', t => { t.plugin.version = '1.2'; }, /stable MAJOR\.MINOR\.PATCH/],
    ['plugin prerelease version', t => { t.plugin.version = '1.2.3-rc.1'; }, /stable MAJOR\.MINOR\.PATCH/],
    ['plugin licence differs', t => { t.plugin.license = 'MIT'; }, /license` "MIT" does not match/],
    ['plugin licence missing', t => { delete t.plugin.license; }, /`license` must be a non-empty/],
    ['plugin declares hooks', t => { t.plugin.hooks = './hooks/hooks.json'; }, /`hooks` must not be declared/],
    ['hooks file missing', t => { t.hooks = null; }, /hooks\.json: file is missing/],
    ['hooks file not JSON', t => { t.hooks = '{"hooks": '; }, /hooks\.json: is not valid JSON/],
    ['hooks key not an object', t => { t.hooks.hooks = []; }, /`hooks` must be an object/],
    ['hooks description missing', t => { delete t.hooks.description; }, /`description` must be a non-empty/],
    ['unexpected top-level key', t => { t.hooks.extra = true; }, /unexpected top-level key "extra"/],
    ['unexpected event', t => { t.hooks.hooks.UserPromptSubmit = [{ hooks: [{ ...INGEST_HOOK }] }]; }, /unexpected hook event "UserPromptSubmit"/],
    ['PostToolUse missing', t => { delete t.hooks.hooks.PostToolUse; }, /hooks\.PostToolUse must be an array of exactly one/],
    ['ingest command changed', t => { t.hooks.hooks.Stop[0].hooks[0].command = 'cvc ingest claude-code'; }, /hooks\.Stop\[0\]\.hooks\[0\] must be exactly/],
    ['ingest timeout changed', t => { t.hooks.hooks.SessionEnd[0].hooks[0].timeout = 5; }, /hooks\.SessionEnd\[0\]\.hooks\[0\] must be exactly/],
    ['ingest hook made async', t => { t.hooks.hooks.PostToolUse[0].hooks[0].async = true; }, /hooks\.PostToolUse\[0\]\.hooks\[0\] must be exactly/],
    ['ingest event with matcher', t => { t.hooks.hooks.PostToolUse[0].matcher = 'Bash'; }, /hooks\.PostToolUse\[0\] has unexpected key "matcher"/],
    ['ingest event with two groups', t => { t.hooks.hooks.SessionEnd.push({ hooks: [{ ...INGEST_HOOK }] }); }, /hooks\.SessionEnd must be an array of exactly one/],
    ['ingest group with two hooks', t => { t.hooks.hooks.Stop[0].hooks.push({ ...INGEST_HOOK }); }, /hooks\.Stop\[0\]\.hooks must be an array of exactly one/],
    ['SessionStart without matcher', t => { delete t.hooks.hooks.SessionStart[0].matcher; }, /SessionStart\[0\]\.matcher must be "startup\|resume"/],
    ['SessionStart wrong timeout', t => { t.hooks.hooks.SessionStart[0].hooks[0].timeout = 10; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart command unquoted', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = 'sh ${CLAUDE_PLUGIN_ROOT}/scripts/check-cvc.sh'; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart command changed', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = 'true'; }, /hooks\.SessionStart\[0\]\.hooks\[0\] must be exactly/],
    ['SessionStart runs ingest', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = `${SESSION_START_HOOK.command} && cvc ingest claude-code --hook`; }, /must only check the CLI and must not run `ingest`/],
    ['SessionStart script missing', t => { t.extraFiles[scriptFile] = null; }, /hooks\.SessionStart references "\$\{CLAUDE_PLUGIN_ROOT\}\/scripts\/check-cvc\.sh", but plugins\/claude-code\/scripts\/check-cvc\.sh does not exist/],
    ['SessionStart script renamed', t => { t.hooks.hooks.SessionStart[0].hooks[0].command = 'sh "${CLAUDE_PLUGIN_ROOT}/scripts/other.sh"'; }, /scripts\/other\.sh does not exist/],
    ['script outside the plugin', t => { t.hooks.hooks.Stop[0].hooks[0].command = 'sh ${CLAUDE_PLUGIN_ROOT}/../outside.sh'; }, /outside the plugin directory/],
    ['script missing', t => { t.hooks.hooks.SessionEnd[0].hooks[0].command = '"${CLAUDE_PLUGIN_ROOT}/scripts/ingest.sh"'; }, /scripts\/ingest\.sh does not exist/],
  ];
  if (posixShell) {
    // These run the SessionStart script, so each one breaks the fixture script in a single way.
    const script = (name, content, expected) => cases.push([name, t => { t.extraFiles[scriptFile] = content; }, expected]);
    script('script silent without cvc', '#!/bin/sh\nexit 0\n', /with no cvc on PATH must print exactly one JSON object/);
    script('script ignores PATH without cvc', edited('is not on the PATH Claude Code runs hooks with', 'is missing'), /with no cvc on PATH must print a `systemMessage` that mentions PATH/);
    script('script noisy when the probe passes', edited('if ! printf', 'if printf'), /accepts the probe must print nothing/);
    script('script silent when the probe fails', checkScript.split('\n').map(line => (line.includes('older than') ? '  :' : line)).join('\n'), /fails the probe must print exactly one JSON object/);
    script('script omits older', edited('is older than', 'differs from'), /fails the probe must print a `systemMessage` that mentions "older"/);
    script('script fails under a path with a space', edited('#!/bin/sh\n', '#!/bin/sh\ncase "$0" in *" "*) exit 3 ;; esac\n'), /containing a space, with a cvc that fails the probe must exit 0; exited 3/);
    script('script exits non-zero', edited('fi\nexit 0\n', 'fi\nexit 1\n'), /must exit 0; exited 1/);
    script('script writes to stderr', edited('fi\nexit 0\n', "fi\nprintf 'oops\\n' >&2\nexit 0\n"), /must write nothing to stderr/);
    script('script probes with other arguments', edited('cvc ingest claude-code --hook', 'cvc ingest claude-code'), /must probe cvc exactly once with `ingest claude-code --hook`; the stub saw "ingest claude-code\\n"/);
    script('script probe keeps the session project directory', edited('CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe cvc ingest', 'cvc ingest'), /must run the probe with CLAUDE_PROJECT_DIR=\/nonexistent\/cvc-plugin-probe, so it does not depend on the session's project; the stub saw "CLAUDE_PROJECT_DIR=[^"]*cvc-hook-[^"]*project\\n"/);
    script('script probe overrides with another directory', edited('CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe cvc ingest', 'CLAUDE_PROJECT_DIR=/tmp cvc ingest'), /must run the probe with CLAUDE_PROJECT_DIR=\/nonexistent\/cvc-plugin-probe.*the stub saw "CLAUDE_PROJECT_DIR=\/tmp\\n"/);
    script('script probes twice', edited('fi\nexit 0\n', 'fi\ncvc ingest claude-code --hook </dev/null >/dev/null 2>&1\nexit 0\n'), /must probe cvc exactly once/);
    script('script never probes', '#!/bin/sh\ncommand -v cvc >/dev/null 2>&1 || printf \'%s\\n\' \'{"systemMessage":"PATH"}\'\nexit 0\n', /must probe cvc exactly once with `ingest claude-code --hook`; the stub saw ""/);
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
      t.extraFiles['plugins/claude-code/scripts/ingest.sh'] = '#!/bin/sh\n';
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
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
  console.log(`Claude Code plugin checker self-tests passed (${cases.length + 3} scenarios${posixShell ? '' : '; shell behaviour skipped, no POSIX sh'}).`);
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
