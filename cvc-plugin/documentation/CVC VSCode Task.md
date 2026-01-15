# CVC VS Code Extension: Task Breakdown

## 1. Project Setup & Scaffolding

- [ ] **Task 1.1: Extension Skeleton**
    
    - [ ] Initialize project with `yo code` (TypeScript + Webpack).
        
    - [ ] Configure `esbuild` for minification.
        
    - [ ] Set up debugging launch configuration for "Extension + Server".
        
- [ ] **Task 1.2: LSP Client Implementation**
    
    - [ ] Install `vscode-languageclient`.
        
    - [ ] Implement logic to locate/download the `cvc-lsp` binary.
        
    - [ ] Implement `LanguageClient` activation logic.
        
    - [ ] **Deliverable:** Extension starts and successfully launches the Rust binary.
        

## 2. Feature: Native Chat Participant (`@cvc`)

- [ ] **Task 2.1: Participant Registration**
    
    - [ ] Register `cvc.chat` in `package.json`.
        
    - [ ] Implement `vscode.chat.createChatParticipant`.
        
- [ ] **Task 2.2: Delegation Logic**
    
    - [ ] Query `vscode.lm.selectChatModels` to find available models (Copilot/GPT-4).
        
    - [ ] Implement the request forwarding loop (User Prompt -> LM -> Stream Response).
        
- [ ] **Task 2.3: Telemetry Dispatch**
    
    - [ ] Construct `StartTurn` payload (User prompt, Active Editor context).
        
    - [ ] Send `$/cvc/turn/start` via LSP.
        
    - [ ] Accumulate LM response stream.
        
    - [ ] Send `$/cvc/turn/end` via LSP upon completion.
        

## 3. Feature: Cognitive Timeline (Tree View)

- [ ] **Task 3.1: Tree Data Provider**
    
    - [ ] Create `TimelineTreeProvider` implementing `vscode.TreeDataProvider`.
        
    - [ ] Define Tree Items: `PendingGroup`, `CommitGroup`, `InteractionItem`.
        
    - [ ] Implement `getTreeItem` and `getChildren`.
        
- [ ] **Task 3.2: Data Fetching (LSP Communication)**
    
    - [ ] Add `sendRequest("cvc/timeline/get")` wrapper to the Language Client.
        
    - [ ] Implement `refresh()` method triggered by `cvc/timeline/refresh` notification from server.
        
    - [ ] Register command `cvc.refreshTimeline` for manual refresh.
        
- [ ] **Task 3.3: UI Interaction**
    
    - [ ] Implement "Click" handler for Tree Items.
        
    - [ ] Logic: When clicked, execute command `cvc.openThoughtDetail` with the Interaction ID.
        

## 4. Feature: Thought Detail View (Webview)

- [ ] **Task 4.1: Webview Panel Logic**
    
    - [ ] Register command `cvc.openThoughtDetail`.
        
    - [ ] Create/Focus a Webview Panel in the editor area (column 2 usually).
        
    - [ ] Implement fetching full details for `InteractionId` from LSP (may need a new LSP method `cvc/interaction/get` or reuse existing).
        
- [ ] **Task 4.2: React/HTML Content**
    
    - [ ] Create a simple React or pure HTML template for the thought detail.
        
    - [ ] Use `@vscode/webview-ui-toolkit` for native buttons/colors.
        
    - [ ] Render Markdown content (Prompt/Response) using a sanitizer (e.g., `dompurify`).
        

## 5. Polish & Packaging

- [ ] **Task 5.1: Icons & Assets**
    
    - [ ] Create high-res icons for the Activity Bar and Tree Items.
        
    - [ ] Design the Extension Marketplace banner.
        
- [ ] **Task 5.2: Configuration**
    
    - [ ] Add settings: `cvc.lspPath` (custom binary path), `cvc.trace.server` (debug logging).
        
    - [ ] Add "Welcome" walk-through for first-time users.