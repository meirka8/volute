import { isReleaseOrLater, readCvcVersion } from "./cvcVersion";

/**
 * The two ways "Share with Remote in Terminal" (`volute.shareRemote`) runs:
 *
 * - `conversations-view`: runs `cvc conversations` in a terminal. Its
 *   interactive view picks the conversation and the remote, and shares,
 *   unshares and publishes through the CLI's own typed phrases.
 * - `input-boxes`: asks for a conversation id and a remote in two input boxes,
 *   then runs `cvc share <id> --remote <remote>` in a terminal.
 */
export type ShareFlow = "conversations-view" | "input-boxes";

/**
 * The first `cvc` release whose `cvc conversations` opens the interactive view
 * (#112). 0.7.0 and earlier only print a list, which shares nothing.
 */
export const CONVERSATIONS_VIEW_FIRST_RELEASE = [0, 7, 1] as const;

/**
 * Whether the view can share, unshare and publish on this platform. It can
 * where the CLI discards type-ahead before a typed phrase, which is Unix
 * (`consent_actions_allowed` in cvc-cli/src/commands/conversations.rs); on
 * Windows it only browses, so the input boxes stay the way to share there.
 * Any other platform keeps the input boxes too, as the extension is not known
 * to run on it.
 */
export function conversationsViewCanShare(platform: NodeJS.Platform): boolean {
  return platform === "linux" || platform === "darwin";
}

/** Whether the CLI that reported `cliVersion` (`undefined`: unknown) has the view. */
export function cliHasConversationsView(cliVersion: string | undefined): boolean {
  return cliVersion !== undefined && isReleaseOrLater(cliVersion, CONVERSATIONS_VIEW_FIRST_RELEASE);
}

/**
 * Which flow the share command runs: the conversations view on Linux and macOS
 * when the CLI has it, the input boxes everywhere else. A CLI whose version is
 * unknown is treated as one without the view, so the flow that works with every
 * release is the one that stays.
 */
export function chooseShareFlow(platform: NodeJS.Platform, cliVersion: string | undefined): ShareFlow {
  return conversationsViewCanShare(platform) && cliHasConversationsView(cliVersion)
    ? "conversations-view"
    : "input-boxes";
}

/** The flow, and the CLI version it was chosen from (`undefined` when it was not read or not known). */
export interface ShareFlowChoice {
  flow: ShareFlow;
  cliVersion: string | undefined;
}

/**
 * Choose the flow for the CLI at `cvcBinary` (`undefined`: not resolved). The
 * version is read with `cvc --version` only where it can change the answer, so
 * Windows, and a machine whose CLI was not found, never start the probe.
 */
export async function resolveShareFlow(
  platform: NodeJS.Platform,
  cvcBinary: string | undefined,
  readVersion: (cvcBinary: string) => Promise<string | undefined> = readCvcVersion,
): Promise<ShareFlowChoice> {
  const cliVersion = cvcBinary !== undefined && conversationsViewCanShare(platform)
    ? await readVersion(cvcBinary)
    : undefined;
  return { flow: chooseShareFlow(platform, cliVersion), cliVersion };
}
