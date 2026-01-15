import * as vscode from 'vscode';

/**
 * Tree item types for the Cognitive Timeline
 */
export type TimelineItemType = 'pending-group' | 'commit-group' | 'interaction';

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
    type: 'pending-group';
    count: number;
}

/**
 * Commit group - contains interactions linked to a specific commit
 */
export interface CommitGroupData extends TimelineItemData {
    type: 'commit-group';
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
    type: 'interaction';
    id: string;
    promptPreview: string;
    timestamp: number;
    author: string;
    parentType: 'pending' | 'commit';
    parentId?: string; // commit SHA if parent is commit
}

/**
 * Union type for all tree item data
 */
export type AnyTimelineItemData = PendingGroupData | CommitGroupData | InteractionItemData;

/**
 * Timeline Tree Item - wraps VS Code TreeItem with our data
 */
export class TimelineTreeItem extends vscode.TreeItem {
    constructor(
        public readonly data: AnyTimelineItemData,
        label: string,
        collapsibleState: vscode.TreeItemCollapsibleState
    ) {
        super(label, collapsibleState);
        this.contextValue = data.type;
        this.setupItem();
    }

    private setupItem(): void {
        switch (this.data.type) {
            case 'pending-group':
                this.iconPath = new vscode.ThemeIcon('cloud-upload');
                this.description = `${this.data.count} pending`;
                this.tooltip = 'Thoughts that will be linked to the next commit';
                break;

            case 'commit-group':
                this.iconPath = new vscode.ThemeIcon('git-commit');
                this.description = this.data.shortSha;
                this.tooltip = new vscode.MarkdownString(
                    `**${this.data.message}**\n\n` +
                    `Commit: \`${this.data.sha}\`\n` +
                    `Thoughts: ${this.data.thoughtCount}`
                );
                break;

            case 'interaction':
                this.iconPath = new vscode.ThemeIcon(
                    this.data.author === 'human' ? 'comment' : 'hubot'
                );
                this.description = this.formatTimestamp(this.data.timestamp);
                this.tooltip = this.data.promptPreview;
                // Make interaction items clickable
                this.command = {
                    command: 'cvc.openThoughtDetail',
                    title: 'Open Thought Detail',
                    arguments: [this.data.id],
                };
                break;
        }
    }

    private formatTimestamp(timestamp: number): string {
        const date = new Date(timestamp);
        const now = new Date();
        const diffMs = now.getTime() - date.getTime();
        const diffMins = Math.floor(diffMs / 60000);
        const diffHours = Math.floor(diffMs / 3600000);
        const diffDays = Math.floor(diffMs / 86400000);

        if (diffMins < 1) {
            return 'just now';
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
}
