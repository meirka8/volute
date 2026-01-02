# CVC Reviewer: Task Breakdown & Roadmap

## 1. Frontend Doctrines & Guidelines

### The "Linear-Grade" UX Doctrine

- **No Spinners:** Use **Optimistic UI** and **Skeleton Loaders** (via `React.Suspense`) strictly. Ideally, data should be "Stale-While-Revalidate" so the user never sees a blank screen.
    
- **Keyboard First:** Every clickable element must have a hotkey. Mouse usage is a fallback.
    
- **Strict Typing:** No `any`. All GitHub API responses must be typed via Octokit's generated types or Zod schemas.
    
- **Zero-Runtime CSS:** Use Tailwind for everything. Avoid `CSS-in-JS` libraries that add runtime overhead. Animation is handled exclusively by `framer-motion`.
    

### The "Alcatraz" Security Doctrine

- **No Third-Party Analytics:** Do not install Google Analytics, Mixpanel, or Sentry. The app must be an air-gapped logic layer.
    
- **Dependency Diet:** Audit `npm` packages rigorously. Minimize the surface area for supply-chain attacks.
    
- **CSP Compliance:** All inline scripts and styles must be compatible with a strict `Content-Security-Policy`.
    

## 2. Phase 1: The Static Harness (Security & Data)

**Goal:** Establish the secure runtime environment and the data fetching layer.

- [ ] **Task 1.1: Repository & Tooling**
    
    - [ ] Initialize Vite + React 19 + TypeScript.
        
    - [ ] Configure Tailwind CSS v4 (or v3 with nesting).
        
    - [ ] Install core libraries: `lucide-react`, `framer-motion`, `wouter`, `@tanstack/react-query`, `octokit`.
        
    - [ ] **Deliverable:** A "Hello World" app running locally with hot reload.
        
- [ ] **Task 1.2: The "Alcatraz" Auth Layer**
    
    - [ ] Implement `AuthContext`: Manages PAT (Personal Access Token) state.
        
    - [ ] Implement `StorageService`: Encrypted `sessionStorage` wrapper (clears on close).
        
    - [ ] Implement "Stateless Gatekeeper" Client: Logic to exchange OAuth code for Token (calling the external Edge Function).
        
    - [ ] **Deliverable:** A Login Screen that accepts a PAT, validates it against `api.github.com/user`, and persists it securely in memory.
        
- [ ] **Task 1.3: The Data Fetching Layer (Octokit + Query)**
    
    - [ ] Create `GithubClient`: A wrapper around Octokit that injects the Auth Token.
        
    - [ ] Configure `QueryClient`: Set default `staleTime` (e.g., 5 minutes) and retry logic.
        
    - [ ] Implement `usePR` hook: Fetches Pull Request metadata (Base SHA, Head SHA, File List).
        
    - [ ] **Deliverable:** The app can log in and display a raw JSON list of files for a hardcoded repo/PR.
        
- [ ] **Task 1.4: Security Hardening**
    
    - [ ] Configure CSP Meta Tag in `index.html`.
        
    - [ ] Implement "Local Proxy Detector": Logic to ping `localhost:3000/health` on startup to switch modes.
        
    - [ ] **Deliverable:** Browser console confirms strict CSP is active.
        

## 3. Phase 2: The Data Overlay (Logic Engine)

**Goal:** Fetch the "Shadow Graph" from Git Objects and link them to the PR.

- [ ] **Task 2.1: The Blob Fetcher**
    
    - [ ] Implement `useCVCBlobs` hook.
        
    - [ ] Logic: Fetch the Tree for `refs/cvc/main`.
        
    - [ ] Logic: Parallel fetch (`Promise.all`) the JSON blobs (Interaction Nodes) referenced in the tree.
        
    - [ ] **Deliverable:** App loads and logs all "Thoughts" stored in the repo.
        
- [ ] **Task 2.2: The Join Engine (Core Logic)**
    
    - [ ] Implement `InteractionMapper`: A utility class to index interactions by `git_commit_hash`.
        
    - [ ] Implement `CommitRanger`: Logic to list all Commit SHAs between `PR.base` and `PR.head`.
        
    - [ ] Logic: Filter the global "Bag of Thoughts" down to _only_ the interactions relevant to this PR.
        
    - [ ] **Deliverable:** A derived state showing "5 Thoughts found for this PR."
        
- [ ] **Task 2.3: Parsing & Validation**
    
    - [ ] Implement Zod Schemas for Interaction Nodes (Runtime validation of the JSON blobs).
        
    - [ ] Handle "Corrupt/Legacy" nodes gracefully (don't crash the UI).
        

## 4. Phase 3: The UI Polish (UX Implementation)

**Goal:** Build the "Graphite-like" interface.

- [ ] **Task 3.1: The Shell Layout**
    
    - [ ] Build `ThreePaneLayout`:
        
        - Sidebar (Collapsible).
            
        - Main (Flex grow).
            
        - Right Panel (Collapsible/Resizable).
            
    - [ ] Implement `Sidebar`: File Tree component with "Viewed" checkboxes.
        
    - [ ] **Deliverable:** A responsive layout skeleton.
        
- [ ] **Task 3.2: The Diff Viewer (Middle Pane)**
    
    - [ ] Integrate `react-diff-view` or build a custom Monaco-based diff viewer.
        
    - [ ] Style it to match the "High Contrast" dark theme.
        
    - [ ] Implement "Line Click" handlers (for Reverse Blame).
        
- [ ] **Task 3.3: The Shadow Timeline (Right Pane)**
    
    - [ ] Build `TimelineNode` component:
        
        - Avatar + Author Name.
            
        - Prompt Text (Markdown rendering).
            
    - [ ] Build `ReasoningAccordion`: The collapsible "CoT" view using `framer-motion` for smooth height animation.
        
    - [ ] **Deliverable:** A scrolling list of interactions on the right side.
        
- [ ] **Task 3.4: "Reverse Blame" Interaction**
    
    - [ ] Implement `ActiveLineContext`: State tracking which line is currently focused in the Diff.
        
    - [ ] Logic: Auto-scroll the Timeline to the interaction linked to the focused Commit SHA.
        
    - [ ] Visuals: Add the SVG Bezier Curve overlay (optional polish).
        
- [ ] **Task 3.5: Command Palette (`cmdk`)**
    
    - [ ] Install `cmdk`.
        
    - [ ] Register global shortcuts (`Cmd+K`, `j`, `k`, `x`).
        
    - [ ] Implement actions: "Go to File", "Toggle Sidebar", "Approve PR".