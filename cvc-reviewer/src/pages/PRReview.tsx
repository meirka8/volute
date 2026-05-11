import { useState, useCallback, useMemo } from "react";
import { useParams } from "wouter";
import { useQuery } from "@tanstack/react-query";
import { Eye, LogOut, GitPullRequest, ExternalLink } from "lucide-react";

import { ThreePaneLayout } from "../components/ui/ThreePaneLayout";
import { Sidebar, type FileItem } from "../components/sidebar/Sidebar";
import { DiffViewer } from "../components/diff/DiffViewer";
import { ShadowTimeline } from "../components/timeline/ShadowTimeline";
import { Paywall } from "./Paywall";
import {
  CommandPalette,
  type CommandAction,
} from "../components/ui/CommandPalette";
import { useCommandPalette } from "../components/ui/useCommandPalette";
import {
  useKeyboardNavigation,
  useFileNavigation,
} from "../hooks/useKeyboardNavigation";

import { useAuth } from "../auth/AuthContext";
import { PaymentRequiredError } from "../auth/errors";
import { usePR } from "../hooks/usePR";
import { useCVCBlobs } from "../hooks/useCVCBlobs";
import { createGithubClient } from "../api/github";
import { CommitRanger } from "../lib/CommitRanger";
import { InteractionMapper } from "../lib/InteractionMapper";
import type { InteractionNode } from "../types/cvc";

export function PRReviewPage() {
  // URL params
  const params = useParams<{ owner: string; repo: string; pr: string }>();
  const owner = params.owner || "";
  const repo = params.repo || "";
  const prNumber = parseInt(params.pr || "0", 10);

  // Auth
  const { logout, acquireToken, isAuthenticated } = useAuth();

  // UI State
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [viewedFiles, setViewedFiles] = useState<Set<string>>(new Set());
  const [selectedInteractionId, setSelectedInteractionId] = useState<
    string | null
  >(null);

  // Command palette
  const { isOpen: isPaletteOpen, close: closePalette } = useCommandPalette();

  // Fetch PR data
  const { data: prData, isLoading: isPRLoading, error: prError } = usePR(owner, repo, prNumber);

  // Fetch CVC blobs
  const { data: allInteractions, isLoading: isBlobsLoading, error: blobsError } = useCVCBlobs(
    owner,
    repo,
  );

  // Join interactions to PR commits
  const { data: prInteractions, isLoading: isJoinLoading } = useQuery({
    queryKey: [
      "pr-interactions",
      owner,
      repo,
      prNumber,
      allInteractions?.length,
      isAuthenticated,
    ],
    queryFn: async (): Promise<InteractionNode[]> => {
      if (!allInteractions || !isAuthenticated) return [];

      const activeToken = await acquireToken(owner, repo).catch(() => null);
      if (!activeToken) return [];

      const client = createGithubClient(activeToken);
      const ranger = new CommitRanger(client);
      const commits = await ranger.getCommitShas(owner, repo, prNumber);

      const mapper = new InteractionMapper(allInteractions);
      return mapper.getInteractionsForRange(commits);
    },
    enabled: !!allInteractions && isAuthenticated && prNumber > 0,
  });

  const sourceFiles = prData?.files ?? [];
  const files: FileItem[] = sourceFiles.map((f) => ({
      filename: f.filename,
      status: f.status as FileItem["status"],
      additions: f.additions,
      deletions: f.deletions,
      patch: f.patch,
    }));

  const activeSelectedFile = selectedFile ?? files[0]?.filename ?? null;

  const currentFile = useMemo(() => {
    if (!activeSelectedFile) return null;
    return files.find((f) => f.filename === activeSelectedFile) || null;
  }, [activeSelectedFile, files]);

  // File navigation
  const fileNames = useMemo(() => files.map((f) => f.filename), [files]);
  const { getNextFile, getPreviousFile } = useFileNavigation(fileNames);

  // Handlers
  const handleSelectFile = useCallback((filename: string) => {
    setSelectedFile(filename);
  }, []);

  const handleToggleViewed = useCallback((filename: string) => {
    setViewedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(filename)) {
        next.delete(filename);
      } else {
        next.add(filename);
      }
      return next;
    });
  }, []);

  const handleNextFile = useCallback(() => {
    const next = getNextFile(activeSelectedFile);
    if (next) setSelectedFile(next);
  }, [activeSelectedFile, getNextFile]);

  const handlePreviousFile = useCallback(() => {
    const prev = getPreviousFile(activeSelectedFile);
    if (prev) setSelectedFile(prev);
  }, [activeSelectedFile, getPreviousFile]);

  const handleExpandReasoning = useCallback(() => {
    // This would toggle the reasoning accordion on the selected interaction
    // For now, just log - the TimelineNode handles its own expand state
    console.log("Expand reasoning shortcut triggered");
  }, []);

  const handleMarkViewed = useCallback(() => {
    if (activeSelectedFile) {
      handleToggleViewed(activeSelectedFile);
    }
  }, [activeSelectedFile, handleToggleViewed]);

  const handleLineClick = useCallback(
    (lineNumber: number, changeType: string) => {
      console.log("Line clicked:", lineNumber, changeType);
      // Future: implement reverse blame to highlight the interaction that created this line
    },
    [],
  );

  // Keyboard navigation
  useKeyboardNavigation({
    onNext: handleNextFile,
    onPrevious: handlePreviousFile,
    onExpand: handleExpandReasoning,
    onMarkViewed: handleMarkViewed,
    enabled: !isPaletteOpen,
  });

  // Command palette actions
  const commandActions: CommandAction[] = useMemo(
    () => [
      {
        id: "next-file",
        label: "Go to next file",
        shortcut: "J",
        icon: <GitPullRequest size={14} className="text-muted" />,
        onSelect: handleNextFile,
        group: "Navigation",
      },
      {
        id: "prev-file",
        label: "Go to previous file",
        shortcut: "K",
        icon: <GitPullRequest size={14} className="text-muted" />,
        onSelect: handlePreviousFile,
        group: "Navigation",
      },
      {
        id: "mark-viewed",
        label: "Toggle file as viewed",
        shortcut: "Space",
        icon: <Eye size={14} className="text-muted" />,
        onSelect: handleMarkViewed,
        group: "Actions",
      },
      {
        id: "open-github",
        label: "Open PR on GitHub",
        icon: <ExternalLink size={14} className="text-muted" />,
        onSelect: () => {
          window.open(
            `https://github.com/${owner}/${repo}/pull/${prNumber}`,
            "_blank",
          );
        },
        group: "Actions",
      },
      {
        id: "logout",
        label: "Logout",
        icon: <LogOut size={14} className="text-muted" />,
        onSelect: logout,
        group: "Account",
      },
    ],
    [
      handleNextFile,
      handlePreviousFile,
      handleMarkViewed,
      owner,
      repo,
      prNumber,
      logout,
    ],
  );

  // Loading state
  const isLoading = isPRLoading || isBlobsLoading || isJoinLoading;

  // Extract errors from queries
  const errorObj = prData === undefined ? prError : null; // React Query destructuring aliasing check, let's just use useQuery error directly below

  // Check for Payment Required
  if (prError instanceof PaymentRequiredError || blobsError instanceof PaymentRequiredError || errorObj instanceof PaymentRequiredError) {
    return <Paywall />;
  }

  // Error state - show if no valid params
  if (!owner || !repo || !prNumber) {
    return (
      <div className="flex h-screen items-center justify-center p-4 text-ink">
        <div className="rr-panel rounded-[2rem] px-8 py-10 text-center">
          <h1 className="mb-2 font-serif text-2xl font-semibold">Invalid PR URL</h1>
          <p className="text-muted">
            Please use the format: /pr/:owner/:repo/:number
          </p>
        </div>
      </div>
    );
  }

  return (
    <>
      <ThreePaneLayout
        sidebar={
          <Sidebar
            files={files}
            selectedFile={activeSelectedFile}
            onSelectFile={handleSelectFile}
            viewedFiles={viewedFiles}
            onToggleViewed={handleToggleViewed}
          />
        }
        main={
          isLoading ? (
            <div className="h-full flex items-center justify-center">
              <div className="text-center text-muted">
                <div className="animate-pulse">
                  <GitPullRequest size={32} className="mx-auto mb-2" />
                  <p className="text-sm">Loading PR #{prNumber}...</p>
                </div>
              </div>
            </div>
          ) : currentFile ? (
            <DiffViewer
              filename={currentFile.filename}
              patch={currentFile.patch}
              onLineClick={handleLineClick}
            />
          ) : (
            <div className="flex h-full items-center justify-center text-muted">
              <p>Select a file to view</p>
            </div>
          )
        }
        rightPanel={
          <ShadowTimeline
            interactions={prInteractions || []}
            selectedInteractionId={selectedInteractionId}
            onSelectInteraction={setSelectedInteractionId}
            prTitle={prData?.pr?.title}
            isLoading={isLoading}
          />
        }
      />

      <CommandPalette
        isOpen={isPaletteOpen}
        onClose={closePalette}
        actions={commandActions}
        files={files.map((f) => ({
          filename: f.filename,
          isViewed: viewedFiles.has(f.filename),
        }))}
        onSelectFile={handleSelectFile}
      />
    </>
  );
}
