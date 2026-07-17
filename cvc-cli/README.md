# CVC CLI

The Command Line Interface for Cognitive Version Control (CVC).

## Overview

`cvc-cli` provides the core user interface for managing your Cognitive Graph. It allows you to initialize repositories, track interactions, and synchronize with remote storage.

## Installation

```bash
cargo install --path cvc-cli
```

## Commands

### `init`

Initialize CVC in the current Git repository.

```bash
cvc init
```

This commands:
1. Creates `.git/cvc/index.db` (SQLite database).
2. Installs `post-commit` hook to `.git/hooks/`.

### `status`

Show the current state of CVC.

```bash
cvc status
```

Displays:
- Number of recorded interactions.
- Number of "Floating Nodes" (interactions not yet linked to a commit).

### `run` (Process Shim)

Wrap a command execution to capture its input/output and context.

```bash
cvc run -- <command> [args...]
```

Example:
```bash
cvc run -- python script.py
```

This will:
1. Snapshot the current "Context" (dirty files in the repo).
2. Run the command.
3. Record the command as the prompt and stdout as the response.
4. Store it as a Floating Node.

### `log`

Visualize the history of interactions.

```bash
cvc log
```

This lists all interactions stored in the local database.

### Automatic-link window

The post-commit linker considers only recent eligible floating interactions. Configure its window in seconds per repository:

```bash
git config cvc.linkWindow <seconds>
```

Valid values are `0..=2592000` seconds (30 days); the default is `86400` (24 hours). `0` disables automatic linking. Missing, malformed, negative, overflowing, or over-max values safely fall back to the default, and linker failures never block a Git commit.

### `push` / `pull`

Synchronize thoughts with the Git Remote.

```bash
cvc push
cvc pull
```
- `push`: Writes local interactions to `refs/cvc/main` and pushes to origin.
- `pull`: Fetches from `refs/cvc/main` and ingests into local DB.

## Troubleshooting

- **CVC not initialized**: Run `cvc init`.
- **Hooks not running**: Ensure `.git/hooks/post-commit` is executable (`chmod +x`).
- **Sync errors**: Ensure you have permissions to push `refs/cvc/main` to the remote.
