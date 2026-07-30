# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately through [GitHub Private Vulnerability Reporting](https://github.com/meirka8/volute/security/advisories/new). If that form is unavailable, email [dev@cvc.dev](mailto:dev@cvc.dev) with the subject `CVC security report`. Do **not** open a public issue, discussion, or pull request for an undisclosed vulnerability.

Include, where possible:

- the affected component and version or commit;
- impact and realistic attack scenario;
- minimal reproduction steps or a proof of concept;
- any known mitigations; and
- whether the report or its details may be credited publicly.

Do not include real secrets, personal data, or third-party confidential data. Use synthetic test data. Please avoid accessing data that is not yours, disrupting services, or publishing details before maintainers have had a reasonable opportunity to investigate and coordinate a fix.

Maintainers will acknowledge the report in the advisory, assess severity and scope, and coordinate remediation and disclosure there. Response and resolution times depend on complexity; we will communicate status through the private advisory rather than promise a deadline we may not be able to meet.

The repository owner must enable **Settings → Security → Private vulnerability reporting** before launch so the preferred form is operational. Email is the fallback; do not send sensitive details through a public GitHub channel.

## Supported versions

Security fixes are provided for the latest released version. Older releases and unreleased source snapshots may not receive fixes. Upgrade to the latest release before reporting an issue already corrected there.

## Security model reminders

CVC is local-first, not a secret manager or guaranteed erasure system. Captured content can contain sensitive material. Scrubbing is defense in depth and can miss secrets or personal data. Review content before sharing it. Data published into Git can remain in objects, clones, forks, reflogs, caches, or backups even after a tombstone or local deletion. Rotate an exposed credential first.

## Release and test-environment trust

Use installers only from this repository and review downloaded scripts before running them. Release installers verify an archive against the release's SHA-256 manifest and refuse unsafe redirects and archive members. Because the archive and manifest are hosted in the same GitHub release, the checksum detects corruption but is not an independent signature if the release account itself is compromised.

The `uat/` noVNC desktop is a local test harness, not a production service. It uses a generated test-only VNC credential and binds the browser endpoint to host loopback. Do not place production credentials or personal data in the UAT desktop, publish its port beyond loopback, or reuse its generated password.
