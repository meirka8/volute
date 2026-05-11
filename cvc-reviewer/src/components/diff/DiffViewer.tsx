import { useMemo } from "react";
import { parseDiff, Diff, Hunk } from "react-diff-view";

interface DiffViewerProps {
  filename: string;
  patch: string | undefined;
  onLineClick?: (
    lineNumber: number,
    changeType: "insert" | "delete" | "normal",
  ) => void;
  highlightedLine?: number | null;
}

export function DiffViewer({
  filename,
  patch,
  onLineClick: _onLineClick,
  highlightedLine,
}: DiffViewerProps) {
  // TODO: Implement line click handling for reverse blame
  void _onLineClick;
  // Parse the diff
  const files = useMemo(() => {
    if (!patch) return [];

    // GitHub API returns patch without the file header, we need to add it
    const fullDiff = `diff --git a/${filename} b/${filename}
--- a/${filename}
+++ b/${filename}
${patch}`;

    try {
      return parseDiff(fullDiff, { nearbySequences: "zip" });
    } catch (e) {
      console.error("Failed to parse diff:", e);
      return [];
    }
  }, [patch, filename]);

  if (!patch) {
    return (
      <div className="flex h-full items-center justify-center text-muted">
        <div className="text-center">
          <p className="font-serif text-2xl">No diff available</p>
          <p className="text-sm mt-1">This file might be binary or empty</p>
        </div>
      </div>
    );
  }

  if (files.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-danger">
        <div className="text-center">
          <p className="font-serif text-2xl">Failed to parse diff</p>
          <p className="text-sm mt-1">The patch format might be invalid</p>
        </div>
      </div>
    );
  }

  const file = files[0];

  return (
    <div className="rr-panel h-full overflow-auto rounded-[1.5rem]">
      <div className="rr-toolbar sticky top-0 z-10 flex items-center justify-between border-b border-line/80 px-4 py-3 backdrop-blur-xl">
        <span className="font-mono text-sm text-ink">{filename}</span>
        {highlightedLine && (
          <span className="text-xs text-action">
            Line {highlightedLine} linked
          </span>
        )}
      </div>

      {/* Diff content */}
      <div className="diff-view-wrapper">
        <Diff viewType="unified" diffType={file.type} hunks={file.hunks || []}>
          {(hunks) =>
            hunks.map((hunk) => <Hunk key={hunk.content} hunk={hunk} />)
          }
        </Diff>
      </div>

      {/* Empty state for no hunks */}
      {(!file.hunks || file.hunks.length === 0) && (
        <div className="flex h-40 items-center justify-center text-sm text-muted">
          No changes in this file
        </div>
      )}
    </div>
  );
}
