# CVC Reviewer

The web-based review interface for Cognitive Version Control.

## Setup

### Prerequisites
- Node.js 20+
- A GitHub Personal Access Token (PAT)

### Authentication
This application uses the **Alcatraz Protocol**: it is a client-side-only application that communicates directly with GitHub. Your token is stored in memory (obfuscated in Session Storage) and never sent to any backend other than `api.github.com`.

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
