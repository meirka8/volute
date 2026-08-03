# Releasing Volute CVC

This runbook is for maintainers and auditors of the public repository
[`meirka8/volute`](https://github.com/meirka8/volute). GitHub triggers
[`.github/workflows/release.yml`](.github/workflows/release.yml) for any `v*` tag;
`check-release-version` rejects a tag unless it is exact stable
`vMAJOR.MINOR.PATCH` before build or publishing. Prerelease and build metadata are
unsupported until the workflow has proper npm dist-tag and VSCE prerelease handling.
Do not release from an unmerged branch or by manually publishing a package.

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
3. In npm, configure Trusted Publisher for public package `@volute_cvc/cvc-mcp` with
   these exact fields: GitHub owner `meirka8`, repository `volute`, workflow filename
   `release.yml`, environment `npm-publish`, and allowed action `npm publish`.
   The allowed-action field is required by current npm documentation (June 2026). Set
   npm Publishing access to **Require two-factor authentication and disallow tokens**,
   revoke obsolete automation tokens, and keep package owners to the minimum needed.
   Do not create a long-lived npm token.

The public repository's OIDC trusted-publishing flow automatically emits npm
provenance. Accordingly, the workflow deliberately uses `npm publish` without a
`--provenance` argument.

## Prepare a release pull request

Choose a stable SemVer version `X.Y.Z`; the tag will be `vX.Y.Z`. Start from current
`main` and use a focused release branch:

```bash
git checkout main
git pull --ff-only origin main
git checkout -b release/vX.Y.Z
```

Set `X.Y.Z` identically in these six release-version locations, then regenerate the
listed lockfiles:

1. `cvc-core/Cargo.toml`
2. `cvc-cli/Cargo.toml`
3. `cvc-lsp/Cargo.toml`
4. `cvc-mcp/Cargo.toml`
5. `cvc-plugin/package.json` and `cvc-plugin/package-lock.json`
6. `npm/cvc-mcp/package.json`

Also update `Cargo.lock`. The reviewer is tested by CI but is not separately
version-published. Use the package manager to preserve lockfile consistency (for
example, run `npm version X.Y.Z --no-git-tag-version` in `cvc-plugin`); do not create
the release tag from that command.

Prepare the curated GitHub Release body at `docs/releases/vX.Y.Z.md`. This file is
mandatory and is used verbatim as the release body; the workflow deliberately does
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
(cd npm/cvc-mcp && node --check bin/cvc-mcp.js && node --check bin/cvc.js && npm test && npm pack --dry-run)
node .github/scripts/generate-third-party-notices.mjs --check
node .github/scripts/check-release-version.mjs vX.Y.Z
```

The last command is the release version gate: it requires the tag spelling and the
four Cargo manifests, extension manifest, and npm wrapper manifest to match exactly.
Inspect the resulting diff, including both lockfiles. Commit with DCO sign-off, push
the branch, and open a PR to `main`:

```bash
git add cvc-core/Cargo.toml cvc-cli/Cargo.toml cvc-lsp/Cargo.toml cvc-mcp/Cargo.toml Cargo.lock cvc-plugin/package.json cvc-plugin/package-lock.json npm/cvc-mcp/package.json docs/releases/vX.Y.Z.md
git commit -s -m "chore: release vX.Y.Z"
git push origin release/vX.Y.Z
```

Obtain required review and passing PR CI before merging. Every release-preparation
commit must satisfy the DCO requirement in [CONTRIBUTING.md](CONTRIBUTING.md).

### Third-party notices

`THIRD-PARTY-NOTICES.md` is a checked-in mechanical report for every non-workspace
crate resolved by `Cargo.lock` and the production dependencies bundled in the VSIX.
It records package metadata and copies license/copyright/notice files available in
the locked package sources without replacing an SPDX expression. For the exceptional
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
view for `@volute_cvc/cvc-mcp` and in the VS Code Marketplace listing for publisher
`volute`. The remote Git tag check below does **not** check either external registry.
If either version already exists, stop. If the registry or Marketplace is unavailable,
ambiguous, or cannot be authenticated for inspection, fail closed: do not tag until its
absence is confirmed in the relevant UI or registry response.

## Create the release tag

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
git tag -s vX.Y.Z -m "Release vX.Y.Z"
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
tarball entry and cleanup. `uninstall.sh` and
`uninstall.ps1` are not
released until they have been separately hardened. The workflow stages release-only
copies of the installers with their default `CVC_RELEASE_VERSION` set to the exact
release tag; users can still explicitly override it with `CVC_RELEASE_VERSION`.
The scripts and license are staged before `SHA256SUMS.txt` is made, so the manifest
covers every downloadable installer script as well as the archives.
Before the GitHub Release is created, the workflow publishes GitHub build provenance
attestations for every staged release asset, including that manifest.

Only after the GitHub Release succeeds do the isolated `publish-marketplace` and
`publish-npm` jobs run through `vscode-marketplace` and `npm-publish`, respectively.
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
  checksum file, then perform the applicable installer smoke test.
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
- npm shows `@volute_cvc/cvc-mcp@X.Y.Z` with Apache-2.0 license, the `meirka8/volute`
  repository metadata, and provenance; inspect the actual npm tarball to confirm it
  includes `LICENSE`. Only then, in an isolated disposable environment, run a launcher
  smoke test such as `npx --yes @volute_cvc/cvc-mcp@X.Y.Z --help`.
- The VS Code Marketplace shows Apache-2.0 and all four target variants for publisher
  `volute` at the expected version.

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

Marketplace publishing can partially publish target variants. Inspect the Marketplace
channel; publish only missing targets through a reviewed, channel-specific recovery.
For npm, preserve the Trusted Publisher workflow identity (`release.yml`); do not
casually change npm trust. If the original artifacts or provenance cannot be preserved,
release a new patch version. Document partial publication and repair forward. Public
package versions and protected tags must not be overwritten; treat GitHub Release
records/assets as administratively immutable unless Immutable Releases is enabled.
