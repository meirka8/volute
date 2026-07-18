# CVC Reviewer

The web-based review interface for Cognitive Version Control.

## Setup

### Prerequisites
- Node.js 20+
- A GitHub Personal Access Token (PAT)

### Authentication
This application uses the **Alcatraz Protocol**: it runs in the browser and is designed to communicate directly with GitHub. During a session, the GitHub PAT is kept in the browser for that session, including session storage used by the app. In the current implementation, the token is used for requests to `api.github.com`.

#### Required PAT Permissions
If using a **Fine-grained Personal Access Token**, you must grant the following permissions for the target repository:

1.  **Contents**: `Read-only` (Required to fetch the CVC Shadow Graph from `refs/cvc/main`)
2.  **Pull requests**: `Read-only` (Required to fetch PR metadata)
3.  **Metadata**: `Read-only` (Default)

> [!IMPORTANT]
> If you see a 403 error regarding "Resource not accessible by personal access token" when fetching `refs/cvc/main`, you are missing the **Contents** permission.

### Development

```bash
# Install dependencies
npm install

# Run dev server
npm run dev
```

### Build

```bash
npm run build
```

## CVC sync compatibility and retention

The reviewer reads CVC sync formats v1–v4, including legacy node-embedded links and
append-only `links/<interaction-id>/<commit-sha>.json` records. Format v4 adds
destination-scoped tombstones. Tombstones are loaded before nodes and win over every
legacy or sharded node representation, so tombstoned interactions are not rendered.
The reviewer evicts the corresponding `cvc-node` query-cache entry when it observes a
tombstone; it refuses malformed/truncated tombstone trees rather than showing an
incomplete timeline. Immutable cognitive entries may be retained in this browser's
IndexedDB query cache for up to 30 days. Tombstones drive eviction from both memory and
that persisted projection; logout and account/PAT transitions clear all reviewer
React Query and IndexedDB caches.

A tombstone is CVC projection suppression, **not** proof that the underlying Git
object was physically erased. Browser query cache eviction only removes this app's
in-memory/query-cache view; it cannot remove data retained by GitHub, a browser
session, developer tools, downloaded responses, clones, forks, reflogs, caches, or
backups. The PAT is retained in browser session storage in the current implementation;
use a private browser context and clear its session data when appropriate.

Temporal links are shown as **lower confidence** because they are inferred from timing
rather than file context. Interaction IDs are UUIDs, not Git content addresses.
Linking-identity attribution is retained for compatibility but is not a public badge.
