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
  private disposables: vscode.Disposable[] = [];

  constructor(
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
  ) {
    this.outputChannel = outputChannel;
    this.lspClient = lspClient;

    this.initializeGitListener();
  }

  /**
   * Initialize listener for Git repository changes
   */
  private initializeGitListener() {
    try {
      const gitExtension = vscode.extensions.getExtension<any>("vscode.git")?.exports;
      if (gitExtension) {
        const api = gitExtension.getAPI(1);
        if (api) {
          // Listen for repository opens
          const disposable = api.onDidOpenRepository((repo: any) => {
            this.registerRepoListener(repo);
            this.refresh();
          });
          this.disposables.push(disposable);

          // Register for existing repositories
          if (api.repositories) {
            api.repositories.forEach((repo: any) => {
              this.registerRepoListener(repo);
            });
          }
        }
      }
    } catch (e) {
      this.outputChannel.appendLine(`Failed to initialize git listener: ${e}`);
    }
  }

  private registerRepoListener(repo: any) {
    const disposable = repo.state.onDidChange(() => {
      // Refresh when git state changes (commit, checkout, etc)
      this.refresh();
    });
    this.disposables.push(disposable);
  }

  /**
   * Clean up resources
   */
  dispose() {
    this.disposables.forEach(d => d.dispose());
    this.disposables = [];
    this._onDidChangeTreeData.dispose();
  }

  /**
   * Get the current HEAD SHA from the VS Code Git extension
   */
  private getHeadSha(): string | undefined {
    try {
      const gitExtension = vscode.extensions.getExtension<any>("vscode.git")?.exports;
      const api = gitExtension?.getAPI(1);

      if (api && api.repositories && api.repositories.length > 0) {
        // Use the first repository or the one matching active editor
        // For simplicity, we'll use the first one found for now, or improve logic to match workspace folder
        const repo = api.repositories[0];
        if (repo && repo.state && repo.state.HEAD && repo.state.HEAD.commit) {
          return repo.state.HEAD.commit;
        }
      }
    } catch (e) {
      this.outputChannel.appendLine(`Error getting HEAD SHA: ${e}`);
    }
    return undefined;
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
      const headSha = this.getHeadSha();
      const response = await this.lspClient.sendTimelineGet({
        maxItems: 50,
        includeUnbound: true,
        headSha: headSha,
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
        hasPrompt: thought.hasPrompt,
        hasCot: thought.hasCot,
        hasResponse: thought.hasResponse,
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
        hasPrompt: thought.hasPrompt,
        hasCot: thought.hasCot,
        hasResponse: thought.hasResponse,
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
