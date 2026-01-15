import * as vscode from 'vscode';
import { CvcLanguageClient } from '../lsp/client';
import { v4 as uuidv4 } from 'uuid';

/**
 * CVC Chat Participant - The Native Delegate
 *
 * Integrates into VS Code's GitHub Copilot Chat sidebar via the @cvc participant.
 * Intercepts user prompts, logs them to CVC, delegates to the user's LLM,
 * and logs the response.
 */
export class CvcChatParticipant {
    private readonly outputChannel: vscode.OutputChannel;
    private readonly lspClient: CvcLanguageClient;
    private participant: vscode.ChatParticipant | undefined;
    private sessionId: string | undefined;

    constructor(
        outputChannel: vscode.OutputChannel,
        lspClient: CvcLanguageClient
    ) {
        this.outputChannel = outputChannel;
        this.lspClient = lspClient;
    }

    /**
     * Register the chat participant with VS Code
     */
    register(context: vscode.ExtensionContext): void {
        this.participant = vscode.chat.createChatParticipant(
            'cvc.chat',
            this.handleChatRequest.bind(this)
        );

        this.participant.iconPath = new vscode.ThemeIcon('lightbulb');

        // Register for disposal
        context.subscriptions.push(this.participant);

        this.outputChannel.appendLine('CVC Chat Participant registered');
    }

    /**
     * Main handler for chat requests
     */
    private async handleChatRequest(
        request: vscode.ChatRequest,
        context: vscode.ChatContext,
        stream: vscode.ChatResponseStream,
        token: vscode.CancellationToken
    ): Promise<vscode.ChatResult> {
        const turnId = uuidv4();

        // Ensure we have a session
        if (!this.sessionId) {
            this.sessionId = uuidv4();
            await this.startSession();
        }

        // Gather context from the request and active editor
        const contextFiles = this.gatherContextFiles(request);

        this.outputChannel.appendLine(`[Turn ${turnId}] User prompt: ${request.prompt}`);

        // Send turn start notification to LSP
        await this.lspClient.sendTurnStart({
            id: turnId,
            prompt: request.prompt,
            author: 'human',
            contextFiles,
        });

        try {
            // Find available language models
            const models = await vscode.lm.selectChatModels({
                vendor: 'copilot',
            });

            if (models.length === 0) {
                // Try without vendor filter as fallback
                const allModels = await vscode.lm.selectChatModels();
                if (allModels.length === 0) {
                    stream.markdown('No language models available. Please ensure GitHub Copilot is installed and signed in.');

                    await this.lspClient.sendTurnEnd({
                        id: turnId,
                        response: 'Error: No language models available',
                        model: undefined,
                    });

                    return { metadata: { turnId, error: 'no_models' } };
                }
                models.push(...allModels);
            }

            // Select the best available model (first one)
            const model = models[0];
            this.outputChannel.appendLine(`[Turn ${turnId}] Using model: ${model.name} (${model.vendor})`);

            // Build messages for the LM
            const messages = this.buildMessages(request, context);

            // Stream the response
            const response = await this.streamModelResponse(
                model,
                messages,
                stream,
                token,
                turnId
            );

            // Send turn end notification to LSP
            await this.lspClient.sendTurnEnd({
                id: turnId,
                response: response.text,
                chainOfThought: response.chainOfThought,
                model: `${model.vendor}/${model.name}`,
            });

            return {
                metadata: {
                    turnId,
                    model: model.name,
                    promptTokens: response.promptTokens,
                    completionTokens: response.completionTokens,
                },
            };
        } catch (error) {
            const errorMessage = error instanceof Error ? error.message : String(error);
            this.outputChannel.appendLine(`[Turn ${turnId}] Error: ${errorMessage}`);

            stream.markdown(`An error occurred: ${errorMessage}`);

            await this.lspClient.sendTurnEnd({
                id: turnId,
                response: `Error: ${errorMessage}`,
                model: undefined,
            });

            return { metadata: { turnId, error: errorMessage } };
        }
    }

    /**
     * Start a new CVC session
     */
    private async startSession(): Promise<void> {
        const workspaceName = vscode.workspace.workspaceFolders?.[0]?.name ?? 'Untitled';

        await this.lspClient.sendSessionStart({
            title: `Chat Session - ${workspaceName}`,
            timestamp: Date.now(),
        });

        this.outputChannel.appendLine(`Session started: ${this.sessionId}`);
    }

    /**
     * Gather context file paths from the request and active editor
     */
    private gatherContextFiles(request: vscode.ChatRequest): string[] {
        const files: string[] = [];

        // Add files from request references
        for (const ref of request.references) {
            if (ref.id === 'vscode.file' && ref.value instanceof vscode.Uri) {
                files.push(ref.value.fsPath);
            } else if (ref.id === 'vscode.editor' && ref.value instanceof vscode.Location) {
                files.push(ref.value.uri.fsPath);
            }
        }

        // Add active editor file if not already included
        const activeEditor = vscode.window.activeTextEditor;
        if (activeEditor && !files.includes(activeEditor.document.uri.fsPath)) {
            files.push(activeEditor.document.uri.fsPath);
        }

        return files;
    }

    /**
     * Build the message array for the language model
     */
    private buildMessages(
        request: vscode.ChatRequest,
        chatContext: vscode.ChatContext
    ): vscode.LanguageModelChatMessage[] {
        const messages: vscode.LanguageModelChatMessage[] = [];

        // Add system message with CVC context
        messages.push(
            vscode.LanguageModelChatMessage.User(
                'You are a helpful coding assistant. The user is working in VS Code and may reference files or code in their questions.'
            )
        );

        // Add conversation history from chat context
        for (const turn of chatContext.history) {
            if (turn instanceof vscode.ChatRequestTurn) {
                messages.push(
                    vscode.LanguageModelChatMessage.User(turn.prompt)
                );
            } else if (turn instanceof vscode.ChatResponseTurn) {
                // Extract text from response parts
                const responseText = turn.response
                    .map(part => {
                        if (part instanceof vscode.ChatResponseMarkdownPart) {
                            return part.value.value;
                        }
                        return '';
                    })
                    .join('');

                if (responseText) {
                    messages.push(
                        vscode.LanguageModelChatMessage.Assistant(responseText)
                    );
                }
            }
        }

        // Add current request with any file context
        let currentPrompt = request.prompt;

        // Include referenced file contents if available
        for (const ref of request.references) {
            if (ref.id === 'vscode.file' && ref.value instanceof vscode.Uri) {
                // The file content is typically included automatically by VS Code
                // We just note the reference
                currentPrompt = `[File: ${ref.value.fsPath}]\n\n${currentPrompt}`;
            }
        }

        messages.push(vscode.LanguageModelChatMessage.User(currentPrompt));

        return messages;
    }

    /**
     * Stream the model response to the chat UI
     */
    private async streamModelResponse(
        model: vscode.LanguageModelChat,
        messages: vscode.LanguageModelChatMessage[],
        stream: vscode.ChatResponseStream,
        token: vscode.CancellationToken,
        turnId: string
    ): Promise<{
        text: string;
        chainOfThought?: string;
        promptTokens?: number;
        completionTokens?: number;
    }> {
        const chunks: string[] = [];

        try {
            const response = await model.sendRequest(messages, {}, token);

            for await (const chunk of response.text) {
                if (token.isCancellationRequested) {
                    this.outputChannel.appendLine(`[Turn ${turnId}] Cancelled by user`);
                    break;
                }

                chunks.push(chunk);
                stream.markdown(chunk);
            }
        } catch (error) {
            if (error instanceof vscode.LanguageModelError) {
                this.outputChannel.appendLine(
                    `[Turn ${turnId}] LM Error: ${error.message} (code: ${error.code})`
                );
                throw error;
            }
            throw error;
        }

        const fullText = chunks.join('');
        this.outputChannel.appendLine(`[Turn ${turnId}] Response length: ${fullText.length} chars`);

        return {
            text: fullText,
            // Token counts not directly available from the API
        };
    }

    /**
     * Dispose of the chat participant
     */
    dispose(): void {
        this.participant?.dispose();
        this.participant = undefined;
        this.sessionId = undefined;
    }
}
