/** Pure selector used to keep non-file and multi-root workspaces inert. */
export function selectSingleFileWorkspaceRoot<T extends { uri: { scheme: string } }>(
  folders: readonly T[],
): T | undefined {
  return folders.length === 1 && folders[0].uri.scheme === "file"
    ? folders[0]
    : undefined;
}
