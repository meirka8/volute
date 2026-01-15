import * as vscode from "vscode";
import { VoluteLanguageClient } from "../lsp/client";
import { v4 as uuidv4 } from "uuid";

/**
 * Volute VC Chat Participant - The Native Delegate
 *
 * Integrates into VS Code's GitHub Copilot Chat sidebar via the @volute participant.
 * Intercepts user prompts, logs them to Volute, delegates to the user's LLM,
 * and logs the response.
 */
export class VoloteChatParticipant {
  private readonly outputChannel: vscode.OutputChannel;
  private readonly lspClient: VoluteLanguageClient;
  private participant: vscode.ChatParticipant | undefined;
  private sessionId: string | undefined;

  constructor(
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
  ) {
    this.outputChannel = outputChannel;
    this.lspClient = lspClient;
  }

  /**
   * Register the chat participant with VS Code
   */
  register(context: vscode.ExtensionContext): void {
    this.participant = vscode.chat.createChatParticipant(
      "volute.chat",
      this.handleChatRequest.bind(this),
    );

    this.participant.iconPath = new vscode.ThemeIcon("lightbulb");

    // Register for disposal
    context.subscriptions.push(this.participant);

    this.outputChannel.appendLine("Volute Chat Participant registered");
  }

  /**
   * Main handler for chat requests
   */
  private async handleChatRequest(
    request: vscode.ChatRequest,
    context: vscode.ChatContext,
    stream: vscode.ChatResponseStream,
    token: vscode.CancellationToken,
  ): Promise<vscode.ChatResult> {
    const turnId = uuidv4();

    // Ensure we have a session
    if (!this.sessionId) {
      this.sessionId = uuidv4();
      await this.startSession();
    }

    // Gather context from the request and active editor
    const contextFiles = this.gatherContextFiles(request);

    this.outputChannel.appendLine(
      `[Turn ${turnId}] User prompt: ${request.prompt}`,
    );

    // Send turn start notification to LSP
    await this.lspClient.sendTurnStart({
      id: turnId,
      prompt: request.prompt,
      author: "human",
      contextFiles,
    });

    try {
      // Find available language models - try without vendor filter first for broader compatibility
      const models = await vscode.lm.selectChatModels();

      this.outputChannel.appendLine(
        `[Turn ${turnId}] Available models: ${models.map((m) => `${m.vendor}/${m.name} (${m.family})`).join(", ")}`,
      );

      if (models.length === 0) {
        stream.markdown(
          "No language models available. Please ensure GitHub Copilot is installed and signed in.",
        );

        await this.lspClient.sendTurnEnd({
          id: turnId,
          response: "Error: No language models available",
          model: undefined,
        });

        return { metadata: { turnId, error: "no_models" } };
      }

      // Prefer GPT-4 class models, fall back to any available
      let model = models.find(
        (m) =>
          m.family.toLowerCase().includes("gpt-4") ||
          m.family.toLowerCase().includes("claude"),
      );
      if (!model) {
        model = models[0];
      }

      this.outputChannel.appendLine(
        `[Turn ${turnId}] Using model: ${model.name} (vendor: ${model.vendor}, family: ${model.family}, id: ${model.id})`,
      );

      // Build messages for the LM with workspace context
      const messages = await this.buildMessages(request, context);

      // Stream the response
      const response = await this.streamModelResponse(
        model,
        messages,
        stream,
        token,
        turnId,
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
      const errorMessage =
        error instanceof Error ? error.message : String(error);
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
   * Start a new Volute session
   */
  private async startSession(): Promise<void> {
    const workspaceName =
      vscode.workspace.workspaceFolders?.[0]?.name ?? "Untitled";

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
      if (ref.id === "vscode.file" && ref.value instanceof vscode.Uri) {
        files.push(ref.value.fsPath);
      } else if (
        ref.id === "vscode.editor" &&
        ref.value instanceof vscode.Location
      ) {
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
   * Includes workspace context and file contents for richer responses
   */
  private async buildMessages(
    request: vscode.ChatRequest,
    chatContext: vscode.ChatContext,
  ): Promise<vscode.LanguageModelChatMessage[]> {
    const messages: vscode.LanguageModelChatMessage[] = [];

    // Build rich context about the workspace and project
    const workspaceContext = await this.buildWorkspaceContext(request);

    // Add system message with rich context
    messages.push(
      vscode.LanguageModelChatMessage.User(
        `You are a helpful coding assistant working in VS Code. You have access to the user's workspace and can help with coding tasks.

${workspaceContext}

Please provide helpful, accurate responses based on the context provided.`,
      ),
    );

    // Add conversation history from chat context
    for (const turn of chatContext.history) {
      if (turn instanceof vscode.ChatRequestTurn) {
        messages.push(vscode.LanguageModelChatMessage.User(turn.prompt));
      } else if (turn instanceof vscode.ChatResponseTurn) {
        // Extract text from response parts
        const responseText = turn.response
          .map((part) => {
            if (part instanceof vscode.ChatResponseMarkdownPart) {
              return part.value.value;
            }
            return "";
          })
          .join("");

        if (responseText) {
          messages.push(
            vscode.LanguageModelChatMessage.Assistant(responseText),
          );
        }
      }
    }

    // Add current request with any file context
    let currentPrompt = request.prompt;

    // Include referenced file contents
    const fileContents = await this.getReferencedFileContents(request);
    if (fileContents) {
      currentPrompt = `${fileContents}\n\nUser request: ${currentPrompt}`;
    }

    messages.push(vscode.LanguageModelChatMessage.User(currentPrompt));

    return messages;
  }

  /**
   * Build workspace context string with project info
   */
  private async buildWorkspaceContext(
    _request: vscode.ChatRequest,
  ): Promise<string> {
    const parts: string[] = [];

    // Add workspace info
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (workspaceFolders && workspaceFolders.length > 0) {
      parts.push(`Workspace: ${workspaceFolders[0].name}`);
    }

    // Add active editor context
    const activeEditor = vscode.window.activeTextEditor;
    if (activeEditor) {
      const doc = activeEditor.document;
      const relativePath = vscode.workspace.asRelativePath(doc.uri);
      const languageId = doc.languageId;

      parts.push(`\nCurrently open file: ${relativePath} (${languageId})`);

      // Include visible code around cursor
      const selection = activeEditor.selection;
      if (!selection.isEmpty) {
        const selectedText = doc.getText(selection);
        if (selectedText.length < 2000) {
          parts.push(
            `\nSelected code:\n\`\`\`${languageId}\n${selectedText}\n\`\`\``,
          );
        }
      } else {
        // Include some context around the cursor
        const cursorLine = selection.active.line;
        const startLine = Math.max(0, cursorLine - 10);
        const endLine = Math.min(doc.lineCount - 1, cursorLine + 10);
        const range = new vscode.Range(
          startLine,
          0,
          endLine,
          doc.lineAt(endLine).text.length,
        );
        const visibleCode = doc.getText(range);
        if (visibleCode.length < 2000) {
          parts.push(
            `\nCode around cursor (lines ${startLine + 1}-${endLine + 1}):\n\`\`\`${languageId}\n${visibleCode}\n\`\`\``,
          );
        }
      }
    }

    return parts.join("\n");
  }

  /**
   * Get contents of referenced files
   */
  private async getReferencedFileContents(
    request: vscode.ChatRequest,
  ): Promise<string | undefined> {
    const fileParts: string[] = [];

    for (const ref of request.references) {
      try {
        let uri: vscode.Uri | undefined;

        if (ref.id === "vscode.file" && ref.value instanceof vscode.Uri) {
          uri = ref.value;
        } else if (
          ref.id === "vscode.editor" &&
          ref.value instanceof vscode.Location
        ) {
          uri = ref.value.uri;
        }

        if (uri) {
          const doc = await vscode.workspace.openTextDocument(uri);
          const content = doc.getText();
          const relativePath = vscode.workspace.asRelativePath(uri);
          const languageId = doc.languageId;

          // Limit file size to avoid token limits
          if (content.length < 10000) {
            fileParts.push(
              `File: ${relativePath}\n\`\`\`${languageId}\n${content}\n\`\`\``,
            );
          } else {
            // Include truncated version
            fileParts.push(
              `File: ${relativePath} (truncated, ${content.length} chars)\n\`\`\`${languageId}\n${content.substring(0, 5000)}\n...[truncated]...\n\`\`\``,
            );
          }
        }
      } catch (error) {
        this.outputChannel.appendLine(
          `Failed to read referenced file: ${error}`,
        );
      }
    }

    return fileParts.length > 0 ? fileParts.join("\n\n") : undefined;
  }

  /**
   * Stream the model response to the chat UI
   */
  private async streamModelResponse(
    model: vscode.LanguageModelChat,
    messages: vscode.LanguageModelChatMessage[],
    stream: vscode.ChatResponseStream,
    token: vscode.CancellationToken,
    turnId: string,
  ): Promise<{
    text: string;
    chainOfThought?: string;
    promptTokens?: number;
    completionTokens?: number;
  }> {
    const chunks: string[] = [];

    try {
      this.outputChannel.appendLine(
        `[Turn ${turnId}] Sending request with ${messages.length} messages`,
      );

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
          `[Turn ${turnId}] LM Error: ${error.message} (code: ${error.code}, cause: ${error.cause})`,
        );
        // Provide more context about the error
        if (
          error.code === "model_not_supported" ||
          error.message.includes("model_not_supported")
        ) {
          this.outputChannel.appendLine(
            `[Turn ${turnId}] Model ${model.name} (${model.id}) is not supported for chat requests`,
          );
        }
        throw error;
      }
      this.outputChannel.appendLine(`[Turn ${turnId}] Unknown error: ${error}`);
      throw error;
    }

    const fullText = chunks.join("");
    this.outputChannel.appendLine(
      `[Turn ${turnId}] Response length: ${fullText.length} chars`,
    );

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
