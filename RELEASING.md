# Releasing Volute CVC

This runbook is for maintainers and auditors of the public repository
[`meirka8/volute`](https://github.com/meirka8/volute). GitHub triggers
[`.github/workflows/release.yml`](.github/workflows/release.yml) for any `v*` tag;
`check-release-version` rejects a tag unless it is exact stable
`vMAJOR.MINOR.PATCH` before build or publishing. Prerelease and build metadata are
unsupported until the workflow has proper npm dist-tag and VSCE prerelease handling.
Do not release from an unmerged branch or by manually publishing a package. The one
exception is [bootstrapping a new npm package name](#bootstrapping-a-new-npm-package-name),
which publishes only an empty placeholder and is not a release.

For contribution, DCO, and security-reporting rules, see
[CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and the
[README](README.md).

## One-time repository and registry setup

Repository administrators must configure the following before a release:

1. Protect `v*` tags. Only the release authority may create them; release tags must
   not be deleted or moved. Separate the tag creator and approver where possible.
2. Create protected GitHub environments named `vscode-marketplace` and `npm-publish`.
   Restrict both to `v*` tags, disable administrator bypass, configure required
   reviewers, and prevent self-review where staffing permits. Required approvals occur
   for every release according to these protection rules. Put the VS Code Marketplace
   credential in the **environment-scoped** `VSCE_PAT` secret for
   `vscode-marketplace`. The token authorizes publisher `volute`; never place its
   value, or any other secret value, in this document, source, issues, or logs.
3. In npm, configure Trusted Publisher for both public packages, `@volute_cvc/cvc-mcp`
   and `@volute_cvc/cvc`, with these exact fields, identical for the two: GitHub owner
   `meirka8`, repository `volute`, workflow filename `release.yml`, environment
   `npm-publish`, and allowed action `npm publish`.
   The allowed-action field is required by current npm documentation (June 2026). For
   each package, set npm Publishing access to **Require two-factor authentication and
   disallow tokens**, revoke obsolete automation tokens, and keep package owners to the
   minimum needed. Do not create a long-lived npm token. A package must already exist
   before its Trusted Publisher can be configured; for a name that has never been
   published, follow [Bootstrapping a new npm package name](#bootstrapping-a-new-npm-package-name)
   first.
4. Configure release tag signing on the ceremony machine. Release tags are
   SSH-signed as the dedicated release identity `Volute Release <dev@cvc.dev>`
   using the release signing key with fingerprint
   `SHA256:oToKgqkDyVREuw1bvYIgdQ2ywcrrn2GH1Rqh01iaEig`; verifiers may pin that
   fingerprint (every SSH signature also embeds its public key). The private
   key's custody and location are deliberately not documented here. On the
   machine that will perform the tag ceremony, set repository-local
   configuration and register the allowed signer:

   ```bash
   git config gpg.format ssh
   git config user.signingkey <path-to-release-signing-public-key>
   git config gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers
   ```

   The allowed-signers file must contain a line binding the release principal
   to the key: `dev@cvc.dev <key-type> <release-public-key>`. The tag command
   in this runbook passes the release identity explicitly with `-c` flags, so
   the machine's normal commit identity is never changed.

The public repository's OIDC trusted-publishing flow automatically emits npm
provenance. Accordingly, the workflow deliberately uses `npm publish` without a
`--provenance` argument.

## Bootstrapping a new npm package name

The launcher is published under two names (`@volute_cvc/cvc-mcp` and `@volute_cvc/cvc`,
both built from `npm/cvc-mcp`). A name that has never been published cannot be released
by the workflow: Trusted Publisher is configured on a package that already exists, and
the workflow holds no token that could create one. So a new name is set up once, by
hand, before the first release that publishes it. Use this procedure for the next name
too.

This bootstrap is **not a release**. It publishes a placeholder at version `0.0.0` that
contains no code and no `bin`, never a version of the product, so it does not conflict
with the rule above against releasing by manually publishing a package. The first real
version still comes from the release workflow, with provenance.

A maintainer does this from their own signed-in npm session, which asks for two-factor
authentication. Never create a token for it.

1. Check the name is free: `npm view <name>` must fail with E404.
2. In a scratch directory outside the repository, write a placeholder package: a
   `package.json` with the exact new name, version `0.0.0`, a description saying it is
   a placeholder and which version is the first real one, `license` `Apache-2.0`, the
   repository metadata of the real package, and `publishConfig.access` `public`; a short
   `README.md`; and the repository `LICENSE`. It must have no `bin`, no scripts, and no
   other code.
3. Publish it from that directory with `npm publish --access public` and complete the
   two-factor prompt. This plain publish is enough; `npm stage publish` is not needed.
4. npm creates a new package through staged publishing (GitHub changelog, 2026-10-02;
   <https://docs.npmjs.com/staged-publishing>), as it did when `@volute_cvc/cvc` was
   bootstrapped on 2026-10-19. npm immediately creates a public stub version
   `0.0.0-stage` (description "Temporary package placeholder for staged publishing") so
   that the name exists, and holds the real `0.0.0` until it is approved. The stub
   belongs to npm and is harmless: it is a prerelease, so a default install never
   selects it. Leave it alone; npm's documentation does not say whether it is ever
   removed. Approve the staged `0.0.0` on npmjs.com (the package's **Staged Packages**
   tab) or with `npm stage approve <stage-id>`; both ask for two-factor authentication.
   If npm publishes the version directly instead, there is nothing to approve.
5. Wait until `npm view <name>` shows version `0.0.0`. Until the approved version has
   been released, `npm view` and `npm deprecate` report E404 for the name. That is
   expected, not a failed publish.
6. Deprecate the placeholder immediately, so nobody installs it:
   `npm deprecate "<name>@0.0.0" "Placeholder with no code; use <first real version> or later."`
7. Configure Trusted Publisher for the name with the exact fields from step 3 of the
   one-time setup above (owner `meirka8`, repository `volute`, workflow `release.yml`,
   environment `npm-publish`, allowed action `npm publish`). It can be configured once the
   package exists.
8. Set the package's Publishing access to **Require two-factor authentication and
   disallow tokens**.
9. Add the name to `PACKAGES` in `npm/cvc-mcp/scripts/stage-package.mjs`, with the README
   its package page shows (and its own `description`, if it should differ from the
   source manifest's), through a normal reviewed PR, only after steps 1 to 8: the
   release would otherwise try to publish a name it cannot create. Both workflows take
   the names from that list, so no secret or workflow change is needed.

The next release then publishes the first real version, through the `npm-publish`
environment approval like any other.

## Prepare a release pull request

Choose a stable SemVer version `X.Y.Z`; the tag will be `vX.Y.Z`. Start from current
`main` and use a focused release branch:

```bash
git checkout main
git pull --ff-only origin main
git checkout -b release/vX.Y.Z
```

Set `X.Y.Z` identically in these eight release-version locations, then regenerate the
listed lockfiles:

1. `cvc-core/Cargo.toml`
2. `cvc-cli/Cargo.toml`
3. `cvc-lsp/Cargo.toml`
4. `cvc-mcp/Cargo.toml`
5. `cvc-plugin/package.json` and `cvc-plugin/package-lock.json`
6. `npm/cvc-mcp/package.json` (the version of both npm packages, `@volute_cvc/cvc-mcp`
   and `@volute_cvc/cvc`, which is staged from it: there is no second manifest to bump)
7. `plugins/claude-code/.claude-plugin/plugin.json` (the Claude Code plugin; its
   `version` lives only here, and the entry in `.claude-plugin/marketplace.json`
   deliberately carries none)
8. The `cvc` entry's `source.ref` in `.claude-plugin/marketplace.json`, set to `vX.Y.Z`
   (with the `v`: it names the release tag, see the next paragraphs)

Also update `Cargo.lock`. The reviewer is tested by CI but is not separately
version-published. Use the package manager to preserve lockfile consistency (for
example, run `npm version X.Y.Z --no-git-tag-version` in `cvc-plugin`); do not create
the release tag from that command.

The Claude Code plugin's version tracks the CLI release it requires, so it moves with
every other manifest at release time. The `cvc` entry in `.claude-plugin/marketplace.json`
is a `git-subdir` source whose `ref` is the release tag, so the marketplace comes from two
places. Claude Code reads the catalog from `main`, but it fetches the plugin's files from
`plugins/claude-code` at the tag that `ref` names. Merging a change under
`plugins/claude-code` therefore publishes nothing: only a release, which moves `ref` and
the plugin `version` together, ships plugin content. Existing installs update when that
plugin version changes, because Claude Code treats an installed plugin as outdated only
when the `version` in its manifest changes. The plugin and the CLI it needs are tagged
together, so a plugin change never reaches users before its CLI does. Edits to the
catalog itself, such as descriptions, are visible from `main` as soon as they merge.

Between merging the release PR and pushing the tag, the catalog names a tag that does not
exist yet, so installs and updates fail until the tag is pushed. Push it promptly after
the merge. To test unreleased plugin changes, run `claude --plugin-dir plugins/claude-code`.
Adding a local checkout as a marketplace does not test them: the plugin is still fetched
from GitHub at `ref`.

Prepare the curated GitHub Release body at `docs/releases/vX.Y.Z.md`. This file is
mandatory and is used as the release body without generator text transformation; the workflow deliberately does
not enable GitHub-generated release notes. Write the user-facing summary and breaking
changes in it, and include an explicit Markdown `Full Changelog` link. For the first
public GitHub/Apache-2.0 OSS release, link to all commits reachable from the release
tag, for example:

```markdown
[Full Changelog](https://github.com/meirka8/volute/commits/v0.4.2)
```

For later releases, use the explicit comparison range, for example
`https://github.com/meirka8/volute/compare/v0.4.2...vX.Y.Z`. Review this file as part
of the release PR and include it in the signed release-preparation commit.

Run the same relevant checks used by the repository workflows from the repository
root. The directory changes below are intentional:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --verbose -- --skip test_parse_vscode_session
cargo check --workspace --all-targets

(cd cvc-plugin && npm ci && npm exec --no -- vsce --version && npm run check-types && npm run lint && npm run compile && xvfb-run -a npm test)
(cd cvc-reviewer && npm ci && npm run lint && npm test -- --run && npm run build)
(cd npm/cvc-mcp && node --check bin/cvc-mcp.js && node --check bin/cvc.js && node --check bin/cvc-lsp.js && npm test && npm pack --dry-run)
node .github/scripts/generate-third-party-notices.mjs --check
node .github/scripts/check-claude-plugin.mjs --self-test
node .github/scripts/check-claude-plugin.mjs
node .github/scripts/check-release-version.mjs vX.Y.Z
```

The two `check-claude-plugin.mjs` commands are CI's `claude-code-plugin` job. They
check the Claude Code plugin marketplace (`.claude-plugin/marketplace.json`), the
plugin manifest, and `plugins/claude-code/hooks/hooks.json` against the documented
contract. They require the plugin version to equal the `cvc-cli` version, and the
marketplace `ref` to be `v` plus that version. CI does not install Claude Code, so a
maintainer who has it should also run `claude plugin validate .` and
`claude plugin validate plugins/claude-code` before opening the release PR.

CI's `npm-wrapper` job also fails if npm would rewrite the manifest of either published
package while publishing: `npm/cvc-mcp/package.json` itself, or the `@volute_cvc/cvc`
manifest that `npm/cvc-mcp/scripts/stage-package.mjs` derives from it (only the name
and description differ). It checks both with `npm pkg fix` and a publish dry run on the
same npm version that the release job pins, and it checks the registry lookup the
release uses against the live registry. If a manifest check fails, run `npm pkg fix` in
`npm/cvc-mcp` and commit the result.

The last command in the block above is the release version gate: it requires the tag
spelling and the four Cargo manifests, extension manifest, npm wrapper manifest, Claude
Code plugin manifest, and the marketplace entry's `source.ref` (which must be the tag
itself, `vX.Y.Z`) to match exactly (the npm manifest is the one both npm names take
their version from). Inspect the resulting diff, including both lockfiles. Commit with
DCO sign-off, push the branch, and open a PR to `main`:

```bash
git add cvc-core/Cargo.toml cvc-cli/Cargo.toml cvc-lsp/Cargo.toml cvc-mcp/Cargo.toml Cargo.lock cvc-plugin/package.json cvc-plugin/package-lock.json npm/cvc-mcp/package.json plugins/claude-code/.claude-plugin/plugin.json .claude-plugin/marketplace.json docs/releases/vX.Y.Z.md
git commit -s -m "chore: release vX.Y.Z"
git push origin release/vX.Y.Z
```

Obtain required review and passing PR CI before merging. Every release-preparation
commit must satisfy the DCO requirement in [CONTRIBUTING.md](CONTRIBUTING.md).

### Third-party notices

`THIRD-PARTY-NOTICES.md` is a checked-in mechanical report for every non-workspace
crate resolved by `Cargo.lock` and the production dependencies bundled in the VSIX.
It records package metadata and imports license/copyright/notice files available in
the locked package sources without replacing an SPDX expression or substantive text;
for deterministic output it normalizes CRLF to LF, removes trailing horizontal
whitespace per line, and emits one terminal newline. For the exceptional
package archives that ship no conventional evidence file, the generator uses only
the exact matching vendored SPDX canonical text and labels package-supplied
author/contributor metadata without inferring copyright. Before generating
or checking it locally, fetch the exact crate sources and install only production
extension packages without lifecycle scripts:

```bash
cargo fetch --locked
(cd cvc-plugin && npm ci --ignore-scripts --omit=dev)
node .github/scripts/generate-third-party-notices.mjs
node .github/scripts/generate-third-party-notices.mjs --check
```

The notice CI job uses the same prerequisites and fails if an installed production
package lacks an SPDX license field or differs from the lockfile. Review any new or
changed license expression and source evidence as part of the release PR; this
mechanical inventory is not legal advice.

The stable required-check name is **Verify third-party notices**. Repository
administrators must add that check to the GitHub branch/ruleset required checks as
an external pre-merge and pre-release action; changing this workflow alone cannot
enforce a GitHub ruleset.

## Confirm external version availability

Before tagging, check version `X.Y.Z` separately in the npm registry's package-version
view for each npm name, `@volute_cvc/cvc-mcp` and `@volute_cvc/cvc`, and in the VS Code
Marketplace listing for publisher `volute`. The remote Git tag check below does **not**
check any external registry. If any of the three versions already exists, stop. If the
registry or Marketplace is unavailable, ambiguous, or cannot be authenticated for
inspection, fail closed: do not tag until its absence is confirmed in the relevant UI or
registry response.

For the two npm names, this command does the lookup read-only, with the same code the
release uses, and publishes nothing. Run it with npm 12.0.2, the version the release
pins:

```bash
node npm/cvc-mcp/scripts/publish-convergent.mjs --version X.Y.Z --decide-only
```

It must report `would-publish` for both names. A `skip` means that name and version
already exist, so stop. Any other error, including a network failure or a reply from an
npm that words its errors differently, fails the command; treat that as unconfirmed
absence and do not tag.

## Create the release tag

First run the signing preflight. It fails fast if the ceremony machine is missing
the release signing setup from the one-time configuration section — signing
configuration is per-machine and per-checkout state that does not travel with the
repository, so verify it before starting rather than discovering it mid-ceremony:

```bash
test "$(git config --get gpg.format)" = ssh || { echo 'Preflight: gpg.format is not ssh.' >&2; exit 1; }
ssh-keygen -lf "$(git config --get user.signingkey)" | grep -F 'SHA256:oToKgqkDyVREuw1bvYIgdQ2ywcrrn2GH1Rqh01iaEig' || { echo 'Preflight: user.signingkey is not the release signing key.' >&2; exit 1; }
grep -q '^dev@cvc.dev ' "$(git config --get gpg.ssh.allowedSignersFile)" || { echo 'Preflight: allowed signers is missing the release principal.' >&2; exit 1; }
```

After the PR merges, fetch origin, update `main`, and fail unless local `HEAD` is
exactly `origin/main`. Record its SHA, verify the remote tag is absent, and create a
signed tag:

```bash
git fetch --prune origin
git checkout main
git pull --ff-only origin main
release_sha=$(git rev-parse HEAD)
test "$release_sha" = "$(git rev-parse origin/main)" || { printf '%s\n' 'Refusing: HEAD is not origin/main.' >&2; exit 1; }
printf 'Release commit: %s\n' "$release_sha"
if git ls-remote --exit-code --tags origin refs/tags/vX.Y.Z; then
  printf '%s\n' 'Refusing: vX.Y.Z already exists on origin.' >&2
  exit 1
else
  status=$?
  test "$status" -eq 2 || exit "$status" # 2 means no matching remote tag
fi
node .github/scripts/check-release-version.mjs vX.Y.Z
git -c user.name='Volute Release' -c user.email='dev@cvc.dev' tag -s vX.Y.Z -m "Release vX.Y.Z"
git verify-tag vX.Y.Z
test "$(git rev-parse vX.Y.Z^{})" = "$release_sha" || { printf '%s\n' 'Refusing: tag does not point to the recorded commit.' >&2; exit 1; }
git push origin refs/tags/vX.Y.Z:refs/tags/vX.Y.Z
```

Record `release_sha` in the release record. Push only this explicit tag refspec—never
use `git push --tags`. Never move, delete, or recreate a release tag. If a tag or an
external version is already present, stop and investigate rather than reusing it.

## What the tag workflow does

`release.yml` first runs the version gate. It checks out full history, fetches
`origin/main` explicitly, resolves the pushed tag to its commit, and refuses to
continue unless that commit is an ancestor of `origin/main`. It also refuses to run
without `docs/releases/vX.Y.Z.md`. These checks run before any build or publication.
It then builds four binary targets (Linux x64, macOS x64, macOS ARM64, and Windows
x64), packages four target-specific VSIX files, and creates the GitHub Release using
that versioned notes file as its curated body.

The release contains the binary archives, four VSIX assets, `LICENSE`,
`THIRD-PARTY-NOTICES.md`, `install.sh`, `install.ps1`, and `SHA256SUMS.txt`.
Every native archive, VSIX, and npm launcher tarball contains
`THIRD-PARTY-NOTICES.md`. The launcher stages an exact temporary copy during
`npm pack`/`npm publish` and removes it afterward; its package test verifies the
tarball entry and cleanup. Every native archive also contains `LICENSE`, and
`install.sh` and `install.ps1` copy both files into a `share/cvc` directory beside
the install directory (`~/.cvc/share/cvc` by default) and print where; an archive
that lacks them is still installed, with a note. `uninstall.sh` and
`uninstall.ps1` are not
released until they have been separately hardened. The workflow stages release-only
copies of the installers with their default `CVC_RELEASE_VERSION` set to the exact
release tag; users can still explicitly override it with `CVC_RELEASE_VERSION`.
The scripts and license are staged before `SHA256SUMS.txt` is made, so the manifest
covers every downloadable installer script as well as the archives.
Before the GitHub Release is created, the workflow publishes GitHub build provenance
attestations for every staged release asset, including that manifest.

Only after the GitHub Release succeeds do the isolated `publish-marketplace` and
`publish-npm` jobs run through `vscode-marketplace` and `npm-publish`, respectively. The
`publish-npm` job publishes the launcher under both npm names, `@volute_cvc/cvc-mcp` and
then `@volute_cvc/cvc`, so a single `npm-publish` approval covers both. They are built
from the one source directory `npm/cvc-mcp`; `npm/cvc-mcp/scripts/stage-package.mjs`
derives the second package (the same files, its own name, description, and README)
and holds the list of names.
Complete every required release-environment approval, as defined by the protection
rules, only after confirming the tag, commit, version gate, and artifacts.

This sequence is **not atomic**: the GitHub Release exists before external channels.
GitHub Release records and assets are not inherently immutable; treat them as immutable
administratively unless GitHub Immutable Releases is separately enabled with a
compatible draft-to-publish workflow.

## Verify publication

After all jobs complete, verify:

- GitHub Release assets include `LICENSE`, `THIRD-PARTY-NOTICES.md`, `install.sh`, `install.ps1`, and
  `SHA256SUMS.txt`; verify each downloaded installer script and archive against that
  checksum file, then perform the applicable installer smoke test. The installer's
  summary must name the `share/cvc` directory it put `LICENSE` and
  `THIRD-PARTY-NOTICES.md` in, and both files must be there.
- Verify GitHub build provenance for every downloaded release asset (including
  `LICENSE`, the two installer scripts, and `SHA256SUMS.txt`) with:

  ```bash
  gh attestation verify <path> --repo meirka8/volute
  ```

  SHA-256 checksums compare a downloaded asset to the digest in the published
  manifest. For provenance, GitHub Actions uses the workflow job's OIDC identity to
  obtain a short-lived Sigstore signing certificate and publishes a signed SLSA build
  provenance statement that binds the asset digest to this repository and workflow
  run. `gh attestation verify` validates that signature, identity, and digest using
  the GitHub/Sigstore trust model. Use both checks; neither removes the need to trust
  the repository, its workflow, and their configured GitHub protections.
- npm shows both `@volute_cvc/cvc-mcp@X.Y.Z` and `@volute_cvc/cvc@X.Y.Z` with
  Apache-2.0 license, the `meirka8/volute` repository metadata, and provenance. For each
  name, `npm view` reports them (the `predicateType` must be present):

  ```bash
  for name in @volute_cvc/cvc-mcp @volute_cvc/cvc; do
    npm view "$name@X.Y.Z" name version license repository.url dist.attestations.provenance.predicateType
  done
  ```

  Inspect the actual npm tarball of each to confirm it includes `LICENSE`; the two list
  the same files, and only `package.json` (the name and description) and `README.md`
  differ. Then, in an isolated disposable environment, confirm that a global install of
  `@volute_cvc/cvc` yields all three commands (`cvc`, `cvc-mcp`, and `cvc-lsp`; on
  Windows they are the `.cmd` shims in the prefix directory itself), without running
  them:

  ```bash
  smoke_prefix=$(mktemp -d)
  npm install --global --prefix "$smoke_prefix" @volute_cvc/cvc@X.Y.Z
  ls "$smoke_prefix/bin"
  rm -rf "$smoke_prefix"
  ```

  The listing must show `cvc`, `cvc-lsp`, and `cvc-mcp`. Then, still in a disposable
  environment, run a launcher smoke test for each name with its own throwaway home
  directory. The launcher publishes into `~/.cvc/bin` and never downgrades, so on a
  machine that already holds a newer or locally built `cvc` it would run that binary
  instead of X.Y.Z. Run the `cvc` bin, not `cvc-mcp`, which ignores `--help`, starts the
  MCP server, and exits 1 outside a Git repository:

  ```bash
  for package in @volute_cvc/cvc-mcp @volute_cvc/cvc; do
    smoke_home=$(mktemp -d)
    HOME="$smoke_home" npx --yes --package "$package@X.Y.Z" -- cvc --version
    rm -rf "$smoke_home"
  done
  ```

  The output must be `cvc X.Y.Z` for both. Stderr also shows the launcher's download
  lines and `Installed CVC X.Y.Z to <tmp>/.cvc/bin`. On Windows, set `USERPROFILE`
  instead of `HOME`.
- The VS Code Marketplace shows Apache-2.0 and all four target variants for publisher
  `volute` at the expected version.
- The Claude Code plugin has no publish job; the tag is its publication. The `cvc` entry
  in `.claude-plugin/marketplace.json` pins the plugin's files to `vX.Y.Z`, so they become
  installable when the tag is pushed. Once the tag exists, install from the marketplace
  into a clean, disposable configuration directory, so that your own Claude Code
  configuration gains neither the marketplace nor the plugin:

  ```bash
  claude_dir=$(mktemp -d)
  CLAUDE_CONFIG_DIR="$claude_dir" claude plugin marketplace add meirka8/volute
  CLAUDE_CONFIG_DIR="$claude_dir" claude plugin install cvc@volute
  ```

  Find the cached copy of the plugin under `$claude_dir/plugins`. Its
  `.claude-plugin/plugin.json` must report version X.Y.Z, and its files must be the
  tag's: extract `git archive vX.Y.Z plugins/claude-code` into a scratch directory and
  run `diff -r` against the cached copy; any differing or missing file is a failure.
  Then remove `$claude_dir`.

For the first public GitHub/Apache-2.0 OSS release, the expected version is `v0.4.2`.
Existing npm `0.4.1` metadata is immutable and reflects the old `UNLICENSED` license
and old repository metadata. `v0.4.2` will carry Apache-2.0, a bundled license, and
Volute repository metadata. After `0.4.2` is available, deprecate npm `0.4.1` with a
message such as:
`Pre-open-source build; unlicensed. Use >=0.4.2 (Apache-2.0).`

## Incidents and rollback

Before a tag is pushed, stop, fix the release PR, and repeat review and validation.
For a failed external job, first use **Re-run failed jobs** on the original workflow
while its artifacts remain available. Never republish a successful channel.

Marketplace publishing is convergent: the publish step queries the Marketplace for the
targets already live at the release version, publishes only the missing ones, retries
transient gallery timeouts (re-checking liveness before each retry, since a timed-out
upload can succeed server-side), then waits up to 10 minutes, polling every 30 seconds,
for the listing to show every target. It fails only if a target is still not listed,
naming it. **Re-run failed jobs** is therefore the complete recovery for a partial
publication while the run's artifacts remain available. Only if a rerun still reports
targets missing, publish exactly those targets through a reviewed, channel-specific
recovery.
The gallery indexes an accepted upload asynchronously, so a publish that succeeded can
take several minutes to be listed (v0.7.0 took about four). If the job is still red
but its log shows `Published volute.cvc (<target>) vX.Y.Z.` for the missing targets and
the error says they were accepted but not yet listed, that is gallery propagation
delay, not a failed publication. Confirm with
`cd cvc-plugin && npm ci && npm exec --no -- vsce show volute.cvc --json`, which uses
the `vsce` pinned in `cvc-plugin/package-lock.json`, and look for the release version
under each of the four `targetPlatform` values. If all four are listed, the
publication is complete: a rerun is only needed to turn the run green, and it skips
every target (it needs a fresh `vscode-marketplace` approval like any rerun). A target
that instead shows a failed publish and is not listed is a real failure: follow the
recovery above.

npm publishing is convergent per package name. The `publish-npm` job stages both names
from `npm/cvc-mcp` (`@volute_cvc/cvc-mcp`, then `@volute_cvc/cvc`) and, before it
publishes anything, asks the registry whether each `name@X.Y.Z` already exists. It
publishes only the names that are missing, in that order, and stops at the first
failure. Only an `npm view` E404 means "not published"; any other lookup error (a network
failure, a registry error, a reply it does not recognise) fails the job without
publishing, because publishing on a guess is how a rerun would republish a name. If
`npm publish` itself fails, the job asks the registry once more before believing it,
since an upload that timed out can have succeeded. **Re-run failed jobs** is therefore
the complete recovery when one name is out and the other is not, for example
`@volute_cvc/cvc-mcp` published and `@volute_cvc/cvc` failed: the rerun logs
`skip @volute_cvc/cvc-mcp@X.Y.Z: already on the registry` and publishes only the missing
name. It needs a fresh `npm-publish` approval like any rerun. Do not publish the missing
name by hand. Registry reads can briefly lag a publish; if a rerun fails because npm
rejected a version that does exist, wait a few minutes, confirm with `npm view`, and
rerun. To see what a rerun would do without publishing anything, run
`node npm/cvc-mcp/scripts/publish-convergent.mjs --version X.Y.Z --decide-only`.

For npm, preserve the Trusted Publisher workflow identity (`release.yml`); do not
casually change npm trust. If the original artifacts or provenance cannot be preserved,
release a new patch version. Document partial publication and repair forward. Public
package versions and protected tags must not be overwritten; treat GitHub Release
records/assets as administratively immutable unless Immutable Releases is enabled.

If the release signing key may be compromised, stop all releases immediately. Remove
or disable the key's authorization through the normal access process — its GitHub
signing-key registration and any allowed-signers entries — and preserve audit
evidence without exposing secret material. Establish and authorize a replacement key,
then update the published fingerprint in this runbook's signing setup through a
reviewed PR. Investigate every tag and release the key could have signed. Do not
silently retag or rewrite published history; disclose affected artifacts and repair
forward.
