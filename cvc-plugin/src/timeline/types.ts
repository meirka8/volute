import * as vscode from "vscode";

/**
 * Tree item types for the Cognitive Timeline
 * Part of the Volute VC extension
 */
export type TimelineItemType = "pending-group" | "commit-group" | "interaction";

/**
 * Base interface for all timeline tree items
 */
export interface TimelineItemData {
  type: TimelineItemType;
}

/**
 * Pending thoughts group - contains unbound interactions
 */
export interface PendingGroupData extends TimelineItemData {
  type: "pending-group";
  count: number;
}

/**
 * Commit group - contains interactions linked to a specific commit
 */
export interface CommitGroupData extends TimelineItemData {
  type: "commit-group";
  sha: string;
  shortSha: string;
  message: string;
  timestamp: number;
  thoughtCount: number;
}

/**
 * Individual interaction/thought item
 */
export interface InteractionItemData extends TimelineItemData {
  type: "interaction";
  id: string;
  promptPreview: string;
  timestamp: number;
  author: string;
  parentType: "pending" | "commit";
  parentId?: string; // commit SHA if parent is commit
  /** Content type flags - indicates what will be shown in detail view */
  hasPrompt: boolean;
  hasCot: boolean;
  hasResponse: boolean;
}

/**
 * Union type for all tree item data
 */
export type AnyTimelineItemData =
  | PendingGroupData
  | CommitGroupData
  | InteractionItemData;

/**
 * Timeline Tree Item - wraps VS Code TreeItem with our data
 *
 * Icon philosophy (aligned with Volute VC brand):
 * - Pending: Symbol of "staging" or "floating" thoughts awaiting commitment
 * - Commits: Git-oriented icon showing the artifact linkage
 * - Interactions: Thought/conversation icons based on author type
 */
export class TimelineTreeItem extends vscode.TreeItem {
  constructor(
    public readonly data: AnyTimelineItemData,
    label: string,
    collapsibleState: vscode.TreeItemCollapsibleState,
  ) {
    super(label, collapsibleState);
    this.contextValue = data.type;
    this.setupItem();
  }

  private setupItem(): void {
    switch (this.data.type) {
      case "pending-group":
        // Lightbulb represents "floating" ideas/thoughts not yet committed
        this.iconPath = new vscode.ThemeIcon(
          "lightbulb",
          new vscode.ThemeColor("charts.yellow"),
        );
        this.description = `${this.data.count} pending`;
        this.tooltip = new vscode.MarkdownString(
          `**Pending Thoughts**\n\n` +
            `${this.data.count} thought${this.data.count !== 1 ? "s" : ""} awaiting linkage.\n\n` +
            `_These will be bound to your next commit._`,
        );
        break;

      case "commit-group":
        // Git commit icon - represents the artifact linkage
        this.iconPath = new vscode.ThemeIcon(
          "git-commit",
          new vscode.ThemeColor("gitDecoration.addedResourceForeground"),
        );
        this.description = this.data.shortSha;
        this.tooltip = new vscode.MarkdownString(
          `**${this.escapeMarkdown(this.data.message)}**\n\n` +
            `Commit: \`${this.data.sha}\`\n\n` +
            `Linked thoughts: **${this.data.thoughtCount}**`,
        );
        break;

      case "interaction": {
        // Different icons based on author type
        const iconId = this.getAuthorIcon(this.data.author);
        const iconColor =
          this.data.parentType === "pending"
            ? new vscode.ThemeColor("charts.yellow")
            : new vscode.ThemeColor("charts.blue");
        this.iconPath = new vscode.ThemeIcon(iconId, iconColor);

        // Build content type indicators (matching the full view icons)
        const contentIcons = this.getContentTypeIcons(
          this.data.hasPrompt,
          this.data.hasCot,
          this.data.hasResponse,
        );
        this.description = `${contentIcons} ${this.formatTimestamp(this.data.timestamp)}`;

        this.tooltip = new vscode.MarkdownString(
          `**${this.capitalizeFirst(this.data.author)}**\n\n` +
            `${this.escapeMarkdown(this.data.promptPreview)}\n\n` +
            `Contains: ${this.getContentTypeLabels(this.data.hasPrompt, this.data.hasCot, this.data.hasResponse)}\n\n` +
            `_${new Date(this.data.timestamp * 1000).toLocaleString()}_`,
        );
        // Make interaction items clickable
        this.command = {
          command: "volute.openThoughtDetail",
          title: "Open Thought Detail",
          arguments: [this.data.id],
        };
        break;
      }
    }
  }

  /**
   * Get appropriate icon for author type
   */
  private getAuthorIcon(author: string): string {
    switch (author) {
      case "human":
        return "account"; // User/person icon
      case "agent":
        return "hubot"; // Robot/AI icon
      case "system":
        return "terminal"; // System/tool output
      case "external":
        return "inbox"; // External input (tickets, etc.)
      default:
        return "comment"; // Generic fallback
    }
  }

  /**
   * Format timestamp as relative time
   * Note: timestamp is Unix seconds, needs conversion to JS milliseconds
   */
  private formatTimestamp(timestamp: number): string {
    const date = new Date(timestamp * 1000);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) {
      return "just now";
    } else if (diffMins < 60) {
      return `${diffMins}m ago`;
    } else if (diffHours < 24) {
      return `${diffHours}h ago`;
    } else if (diffDays < 7) {
      return `${diffDays}d ago`;
    } else {
      return date.toLocaleDateString();
    }
  }

  /**
   * Escape special markdown characters
   */
  private escapeMarkdown(text: string): string {
    return text.replace(/[*_`[\]]/g, "\\$&");
  }

  /**
   * Get content type icons matching the full detail view
   * Icons: 💬 Prompt, 🧠 CoT, 🤖 Response
   */
  private getContentTypeIcons(
    hasPrompt: boolean,
    hasCot: boolean,
    hasResponse: boolean,
  ): string {
    const icons: string[] = [];
    if (hasPrompt) icons.push("💬");
    if (hasCot) icons.push("🧠");
    if (hasResponse) icons.push("🤖");
    return icons.join("");
  }

  /**
   * Get content type labels for tooltip
   */
  private getContentTypeLabels(
    hasPrompt: boolean,
    hasCot: boolean,
    hasResponse: boolean,
  ): string {
    const labels: string[] = [];
    if (hasPrompt) labels.push("Prompt");
    if (hasCot) labels.push("Chain of Thought");
    if (hasResponse) labels.push("Response");
    return labels.length > 0 ? labels.join(", ") : "Empty";
  }

  /**
   * Capitalize first letter
   */
  private capitalizeFirst(text: string): string {
    return text.charAt(0).toUpperCase() + text.slice(1);
  }
}
