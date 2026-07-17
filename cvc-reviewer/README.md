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

## CVC sync compatibility

The reviewer reads legacy node-embedded artifact links and sync format v3's
append-only `links/<interaction-id>/<commit-sha>.json` records. Temporal links are
shown as **lower confidence** because they are inferred from timing rather than file
context. Linking-identity attribution (the configured repository signature email) is
retained for data compatibility but is not shown as a public timeline badge.
