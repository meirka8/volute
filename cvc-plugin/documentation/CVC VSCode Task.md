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
        

## 2. Feature: Native Chat Participant (`@cvc`) - Legacy / Background Capability

> **Note:** This feature exists, but it is not the recommended getting-started path. The primary VS Code workflow is the passive watcher for VS Code + GitHub Copilot.

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

- [x] **Task 3.1: Tree Data Provider**
    
    - [x] Create `TimelineTreeProvider` implementing `vscode.TreeDataProvider`.
        
    - [x] Define Tree Items: `PendingGroup`, `CommitGroup`, `InteractionItem`.
        
    - [x] Implement `getTreeItem` and `getChildren`.
        
- [x] **Task 3.2: Data Fetching (LSP Communication)**
    
    - [x] Add `sendRequest("cvc/timeline/get")` wrapper to the Language Client.
        
    - [x] Implement `refresh()` method triggered by `cvc/timeline/refresh` notification from server.
        
    - [x] Register command `cvc.refreshTimeline` for manual refresh.
        
- [x] **Task 3.3: UI Interaction**
    
    - [x] Implement "Click" handler for Tree Items.
        
    - [x] Logic: When clicked, execute command `cvc.openThoughtDetail` with the Interaction ID.
        

## 4. Feature: Thought Detail View (Webview)

- [x] **Task 4.1: Webview Panel Logic**
    
    - [x] Register command `cvc.openThoughtDetail`.
        
    - [x] Create/Focus a Webview Panel in the editor area (column 2 usually).
        
    - [x] Implement fetching full details for `InteractionId` from LSP (may need a new LSP method `cvc/interaction/get` or reuse existing).
        
- [x] **Task 4.2: React/HTML Content**
    
    - [x] Create a simple React or pure HTML template for the thought detail.
        
    - [x] Use `@vscode/webview-ui-toolkit` for native buttons/colors.
        
    - [x] Render Markdown content (Prompt/Response) using a sanitizer (e.g., `dompurify`).
        

## 5. Polish & Packaging

- [x] **Task 5.1: Icons & Assets**
    
    - [x] Create high-res icons for the Activity Bar and Tree Items.
        
    - [x] Design the Extension Marketplace banner.
        
- [x] **Task 5.2: Configuration**
    
    - [x] Add settings: `cvc.lspPath` (custom binary path), `cvc.trace.server` (debug logging).
        
    - [x] Add "Welcome" walk-through for first-time users.

---

## Notes

### Current Onboarding Direction
The intended primary VS Code workflow is now the passive watcher for **VS Code + GitHub Copilot**. The separate chat participant remains legacy behavior and should not be treated as the recommended getting-started path in user-facing guides.

### Branding Update
The extension has been rebranded from "CVC" to **Volute VC** as per the branding guidelines in `/branding_and_design/`. Key changes:

- Extension name: `volute-vc`
- Display name: `Volute VC`
- Chat participant: `@volute`
- Command prefix: `volute.*`
- Settings prefix: `volute.*`

### Brand Colors Applied
The Volute VC brand colors have been applied to the webview:
- **Git Orange** (`#F05032`) - Response section accent, errors
- **Electric Teal** (`#64FFDA`) - Prompt section accent, buttons, links
- **Cognitive Navy** (`#0A192F`) - Backgrounds
- **Void Slate** (`#112240`) - Secondary backgrounds
- **Text White** (`#E6F1FF`) - Primary text
- **Text Muted** (`#8892B0`) - Secondary text

### Assets Created
- `resources/icon.svg` - Extension icon (colored, with background)
- `resources/icon.png` - Extension icon for marketplace (128x128)
- `resources/activitybar-icon.svg` - Monochrome icon for Activity Bar
