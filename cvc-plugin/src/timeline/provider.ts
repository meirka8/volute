import * as vscode from "vscode";
import { VoluteLanguageClient } from "../lsp/client";
import {
  TimelineTreeItem,
  PendingGroupData,
  CommitGroupData,
  InteractionItemData,
} from "./types";
import { InteractionSummary, CommitWithThoughts } from "../lsp/protocol";

/**
 * Tree Data Provider for the Cognitive Timeline
 *
 * Displays pending thoughts and historical interactions linked to commits.
 */
export class TimelineTreeProvider implements vscode.TreeDataProvider<TimelineTreeItem> {
  private readonly _onDidChangeTreeData = new vscode.EventEmitter<
    TimelineTreeItem | undefined | void
  >();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private readonly outputChannel: vscode.OutputChannel;
  private readonly lspClient: VoluteLanguageClient;

  // Cached data
  private pendingThoughts: InteractionSummary[] = [];
  private commits: CommitWithThoughts[] = [];
  private isLoading = false;

  constructor(
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
  ) {
    this.outputChannel = outputChannel;
    this.lspClient = lspClient;
  }

  /**
   * Refresh the tree data from the LSP server
   */
  async refresh(): Promise<void> {
    if (this.isLoading) {
      this.outputChannel.appendLine(
        "Timeline refresh already in progress, skipping",
      );
      return;
    }

    this.isLoading = true;
    this.outputChannel.appendLine("Refreshing Cognitive Timeline...");

    try {
      const response = await this.lspClient.sendTimelineGet({
        maxItems: 50,
        includeUnbound: true,
      });

      if (response) {
        this.pendingThoughts = response.pending;
        this.commits = response.commits;
        this.outputChannel.appendLine(
          `Timeline loaded: ${this.pendingThoughts.length} pending, ${this.commits.length} commits`,
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.outputChannel.appendLine(`Failed to refresh timeline: ${message}`);
    } finally {
      this.isLoading = false;
      this._onDidChangeTreeData.fire();
    }
  }

  /**
   * Get tree item representation
   */
  getTreeItem(element: TimelineTreeItem): vscode.TreeItem {
    return element;
  }

  /**
   * Get children for a tree item
   */
  async getChildren(element?: TimelineTreeItem): Promise<TimelineTreeItem[]> {
    // Root level - show pending group and commit groups
    if (!element) {
      return this.getRootItems();
    }

    // Children of pending group
    if (element.data.type === "pending-group") {
      return this.getPendingChildren();
    }

    // Children of commit group
    if (element.data.type === "commit-group") {
      return this.getCommitChildren(element.data.sha);
    }

    // Interaction items have no children
    return [];
  }

  /**
   * Get root level items (pending group + commit groups)
   */
  private getRootItems(): TimelineTreeItem[] {
    const items: TimelineTreeItem[] = [];

    // Always show pending group (even if empty, to show context)
    const pendingData: PendingGroupData = {
      type: "pending-group",
      count: this.pendingThoughts.length,
    };
    items.push(
      new TimelineTreeItem(
        pendingData,
        "Pending Thoughts",
        this.pendingThoughts.length > 0
          ? vscode.TreeItemCollapsibleState.Expanded
          : vscode.TreeItemCollapsibleState.Collapsed,
      ),
    );

    // Add commit groups
    for (const commit of this.commits) {
      const commitData: CommitGroupData = {
        type: "commit-group",
        sha: commit.sha,
        shortSha: commit.sha.substring(0, 7),
        message: this.truncateMessage(commit.message),
        timestamp: commit.timestamp,
        thoughtCount: commit.thoughts.length,
      };
      items.push(
        new TimelineTreeItem(
          commitData,
          this.truncateMessage(commit.message),
          commit.thoughts.length > 0
            ? vscode.TreeItemCollapsibleState.Collapsed
            : vscode.TreeItemCollapsibleState.None,
        ),
      );
    }

    return items;
  }

  /**
   * Get children of the pending group
   */
  private getPendingChildren(): TimelineTreeItem[] {
    return this.pendingThoughts.map((thought) => {
      const data: InteractionItemData = {
        type: "interaction",
        id: thought.id,
        promptPreview: this.truncatePrompt(thought.promptPreview),
        timestamp: thought.timestamp,
        author: thought.author,
        parentType: "pending",
      };
      return new TimelineTreeItem(
        data,
        this.truncatePrompt(thought.promptPreview),
        vscode.TreeItemCollapsibleState.None,
      );
    });
  }

  /**
   * Get children of a commit group
   */
  private getCommitChildren(commitSha: string): TimelineTreeItem[] {
    const commit = this.commits.find((c) => c.sha === commitSha);
    if (!commit) {
      return [];
    }

    return commit.thoughts.map((thought) => {
      const data: InteractionItemData = {
        type: "interaction",
        id: thought.id,
        promptPreview: this.truncatePrompt(thought.promptPreview),
        timestamp: thought.timestamp,
        author: thought.author,
        parentType: "commit",
        parentId: commitSha,
      };
      return new TimelineTreeItem(
        data,
        this.truncatePrompt(thought.promptPreview),
        vscode.TreeItemCollapsibleState.None,
      );
    });
  }

  /**
   * Truncate commit message to first line and max length
   */
  private truncateMessage(message: string): string {
    const firstLine = message.split("\n")[0];
    if (firstLine.length > 50) {
      return firstLine.substring(0, 47) + "...";
    }
    return firstLine;
  }

  /**
   * Truncate prompt preview
   */
  private truncatePrompt(prompt: string): string {
    const cleaned = prompt.replace(/\s+/g, " ").trim();
    if (cleaned.length > 60) {
      return cleaned.substring(0, 57) + "...";
    }
    return cleaned;
  }

  /**
   * Handle notification from server that timeline has changed
   */
  onTimelineRefresh(): void {
    this.refresh();
  }
}
