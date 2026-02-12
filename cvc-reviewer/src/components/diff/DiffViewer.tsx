import { useMemo } from "react";
import { parseDiff, Diff, Hunk } from "react-diff-view";
import "react-diff-view/style/index.css";

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
      <div className="h-full flex items-center justify-center text-[#555555]">
        <div className="text-center">
          <p className="text-lg">No diff available</p>
          <p className="text-sm mt-1">This file might be binary or empty</p>
        </div>
      </div>
    );
  }

  if (files.length === 0) {
    return (
      <div className="h-full flex items-center justify-center text-[#ef4444]">
        <div className="text-center">
          <p className="text-lg">Failed to parse diff</p>
          <p className="text-sm mt-1">The patch format might be invalid</p>
        </div>
      </div>
    );
  }

  const file = files[0];

  return (
    <div className="h-full overflow-auto bg-[#080808]">
      {/* File header */}
      <div className="sticky top-0 z-10 bg-[#0d0d0d] border-b border-[#1c1c1c] px-4 py-2 flex items-center justify-between">
        <span className="font-mono text-sm text-[#ededed]">{filename}</span>
        {highlightedLine && (
          <span className="text-xs text-[#5e6ad2]">
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
        <div className="flex items-center justify-center h-40 text-[#555555] text-sm">
          No changes in this file
        </div>
      )}
    </div>
  );
}
