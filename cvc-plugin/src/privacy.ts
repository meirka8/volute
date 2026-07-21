import type { PrivacyStatus } from "./lsp/protocol";

/** Only UUIDs are safe to include in the normal extension output channel. */
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function isSafeInteractionId(value: unknown): value is string {
  return typeof value === "string" && UUID_PATTERN.test(value);
}

/** Reject malformed policy responses rather than granting passive access. */
export function isPrivacyStatus(value: unknown): value is PrivacyStatus {
  if (!value || typeof value !== "object") {
    return false;
  }
  const status = value as Record<string, unknown>;
  return typeof status.captureAcknowledged === "boolean" &&
    typeof status.captureNoticeVersion === "number" &&
    Number.isFinite(status.captureNoticeVersion) &&
    typeof status.passiveCaptureAllowed === "boolean" &&
    typeof status.privateByDefault === "boolean" &&
    typeof status.privateDefaultStatement === "string" &&
    typeof status.sharingSummary === "string" &&
    typeof status.autoPushEnabled === "boolean";
}

/** Passive storage access is permitted only by the current LSP policy result. */
export function mayStartPassiveWatcher(status: PrivacyStatus | undefined, hasWatcher: boolean): boolean {
  return isPrivacyStatus(status) && status.passiveCaptureAllowed && !hasWatcher;
}

export interface StoppableWatcher {
  stop(): void;
}

/**
 * Owns the watcher lifecycle at the privacy boundary. A refresh that cannot
 * positively establish current consent revokes an existing watcher at once.
 */
export class PassiveWatcherGate<T extends StoppableWatcher> {
  private watcher: T | undefined;

  reconcile(status: PrivacyStatus | undefined, create: () => T): T | undefined {
    if (!isPrivacyStatus(status) || !status.passiveCaptureAllowed) {
      this.watcher?.stop();
      this.watcher = undefined;
      return undefined;
    }

    if (!this.watcher) {
      this.watcher = create();
    }
    return this.watcher;
  }

  dispose(): void {
    this.watcher?.stop();
    this.watcher = undefined;
  }
}

/** Workspace storage must match the active workspace URI exactly. */
export function isExactWorkspaceStorage(workspaceData: unknown, workspaceUri: string): boolean {
  if (!workspaceData || typeof workspaceData !== "object") {
    return false;
  }
  const candidate = workspaceData as { folder?: unknown; workspace?: unknown };
  return candidate.folder === workspaceUri || candidate.workspace === workspaceUri;
}
