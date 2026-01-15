# CVC VS Code Extension: Task Breakdown

## 1. Project Setup & Scaffolding

- [x] **Task 1.1: Extension Skeleton**
    
    - [x] Initialize project with `yo code` (TypeScript + Webpack).
        
    - [x] Configure `esbuild` for minification.
        
    - [x] Set up debugging launch configuration for "Extension + Server".
        
- [x] **Task 1.2: LSP Client Implementation**
    
    - [x] Install `vscode-languageclient`.
        
    - [x] Implement logic to locate/download the `cvc-lsp` binary.
        
    - [x] Implement `LanguageClient` activation logic.
        
    - [x] **Deliverable:** Extension starts and successfully launches the Rust binary.
        

## 2. Feature: Native Chat Participant (`@cvc`)

- [x] **Task 2.1: Participant Registration**
    
    - [x] Register `cvc.chat` in `package.json`.
        
    - [x] Implement `vscode.chat.createChatParticipant`.
        
- [x] **Task 2.2: Delegation Logic**
    
    - [x] Query `vscode.lm.selectChatModels` to find available models (Copilot/GPT-4).
        
    - [x] Implement the request forwarding loop (User Prompt -> LM -> Stream Response).
        
- [x] **Task 2.3: Telemetry Dispatch**
    
    - [x] Construct `StartTurn` payload (User prompt, Active Editor context).
        
    - [x] Send `$/cvc/turn/start` via LSP.
        
    - [x] Accumulate LM response stream.
        
    - [x] Send `$/cvc/turn/end` via LSP upon completion.
        

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
