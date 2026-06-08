# Project Design: CVC Reviewer Webapp

## 1. Executive Summary

The **CVC Reviewer** is a visualization layer for Pull Requests that overlays the "Cognitive History" (reasoning, prompts, chain-of-thought) atop the standard "Artifact History" (code diffs).

**Primary Mandates:**

1. **Alcatraz Security:** Direct-connection architecture. The application must operate without an intermediary backend storing source code or tokens. It communicates directly from the Client (Browser) to the Git Host (GitHub/GitLab).
    
2. **Linear-Grade UX:** A "Tool-First" design philosophy focusing on keyboard centricity, sub-100ms latency perception, and information density without clutter.
    

## 2. The UX Doctrine: "Tool-First" (The Linear Method)

We reject the "Enterprise Dashboard" aesthetic (Jira, ServiceNow) in favor of **"Utilitarian Luxury."** The interface is designed for power users who spend hours in the tool.

### 2.1 Core UX Principles

- **The 100ms Rule:** Every interaction (switching files, expanding thoughts) must resolve in under 100ms. If network data is missing, show Optimistic UI (skeleton loaders are a last resort; stale-while-revalidate is preferred).
    
- **Keyboard is King:** The mouse is a fallback.
    
    - `j` / `k`: Navigate commits/thoughts.
        
    - `x`: Expand/Collapse reasoning.
        
    - `Cmd+K`: Command Palette for all actions.
        
- **Contextual Density:**
    
    - _Scan Mode:_ By default, the timeline shows only "Intent" (User Prompts).
        
    - _Focus Mode:_ Hovering or selecting a node reveals "Reasoning" (CoT) and "Context" details.
        
- **High-Contrast Typography:** Use a rigorous type scale. Code is `Monospace`; UI is `Inter`/`San Francisco`. Interactive elements are distinct from read-only text not by color alone, but by position and weight.
    

### 2.2 The "Anti-Bloat" Rules

1. **No Dropdowns:** If there are < 5 options, use a toggle group or visible radio list.
    
2. **No Wizards:** Settings and configurations happen in-place or via the Command Palette.
    
3. **No "Save" Buttons:** All state is persisted immediately to local storage or the URL.
    

## 3. Security Architecture: The "Alcatraz" Protocol

To serve clients with sensitive IP, we adopt a **Client-Side Only (Zero-Backend)** architecture. The web application acts purely as a static harness that runs code in the user's browser.

### 3.1 Data Flow

- **Traditional SaaS:** `User -> SaaS Server (Stores Token) -> GitHub`. **(REJECTED)**
    
- **CVC Alcatraz:** `User Browser (Stores Token in Memory) -> GitHub API`.
    
    - **No Intermediary:** Our servers never see the code, the tokens, or the PRs.
        
    - **Static Hosting:** The app is just HTML/JS/CSS hosted on a CDN (Vercel/Netlify/Cloudflare Pages).
        
    - **Direct API Calls:** All data fetching happens via `fetch()` calls from the user's IP address directly to `api.github.com`.
        

### 3.2 Authentication Strategy

- **Option A: Personal Access Tokens (PAT):**
    
    - _Mechanism:_ User generates a granular PAT (read-only) and pastes it.
        
    - _Pros:_ Zero infrastructure, maximum trust.
        
    - _Cons:_ High friction.
        
- **Option B: The Stateless Gatekeeper (OAuth):**
    
    - _Mechanism:_ A single Edge Function (Cloudflare Worker/Vercel) exists _solely_ to exchange the OAuth `code` for an `access_token` using the hidden `client_secret`.
        
    - _Security:_ The function is **ephemeral and memory-less**. It receives the code, swaps it, returns the token to the browser, and dies. It creates no logs and writes to no database.
        
    - _Pros:_ Low friction (One-click login).
        
    - _Cons:_ Requires maintaining one micro-service.
        
- **Option C: Local Proxy Mode (Enterprise Gold Standard):**
    
    - _Mechanism:_ User runs `cvc ui` in terminal -> spawns `localhost:3000`.
        
    - _Pros:_ Bypasses all browser auth; uses local machine's SSH keys/credentials.
        

### 3.3 Content Security Policy (CSP)

We enforce a strict CSP header that prevents the browser from sending data anywhere except GitHub.

```
default-src 'self';
connect-src 'self' [https://api.github.com](https://api.github.com) http://localhost:3000 [https://auth.cvc.dev](https://auth.cvc.dev);
script-src 'self';
style-src 'self' 'unsafe-inline';

```

_(Note: `https://auth.cvc.dev` added only for the Token Exchange endpoint)_

## 4. Technical Stack

- **Framework:** React 19 (for Compiler optimizations).
    
- **Build System:** Vite (Instant start).
    
- **State Management:** TanStack Query (React Query) - crucial for caching GitHub API responses and handling "stale-while-revalidate".
    
- **Styling:** Tailwind CSS (Utility-first matches the Linear aesthetic).
    
    - _Icons:_ Lucide React (Clean, vector, consistent strokes).
        
- **Animation:** Framer Motion (Layout transitions, "layoutId" for smooth morphing).
    
- **Router:** Wouter (Tiny, no bloat) or TanStack Router (Type-safe).
    

## 5. Feature Specifications & Layout

### 5.1 The Workspace Layout (The "Graphite" View)

A three-pane layout optimized for wide screens.

|   |   |   |
|---|---|---|
|**Left Pane (Navigation)**|**Middle Pane (The Artifact)**|**Right Pane (The Cognition)**|
|**Width:** 250px (Collapsible)|**Width:** Flex (Code)|**Width:** 400px (Resizeable)|
|List of Files in PR|The Code Diff|The "Shadow Timeline"|
|Tree View|Syntax Highlighted|Interaction Nodes|
|Status Icons (Reviewed/New)|Line-number linking|CoT & Prompts|

### 5.2 The "Shadow Timeline" (Right Pane)

This is the heart of CVC Reviewer. It visualizes the `refs/cvc/main` data linked to the currently visible code.

- **Node Representation:**
    
    - _User Node:_ Avatar + Prompt Text (Truncated).
        
    - _Model Node:_ Icon + Response Summary.
        
    - _Hidden Data:_ Chain of Thought is collapsed by default behind a "Reasoning" toggle (like a spoiler tag).
        
- **Reverse Blame (The "Why" Click):**
    
    - When a user clicks a line of code in the Middle Pane (Diff), the Right Pane auto-scrolls to the specific _Interaction Node_ that generated that line.
        
    - _Visual Cue:_ A connecting bezier curve line (using SVG overlay) momentarily draws between the code line and the thought node.
        

### 5.3 Command Palette (`Cmd+K`)

The primary controller.

- `> Go to file...`
    
- `> View reasoning...`
    
- `> Switch to local mode...`
    
- `> Toggle dark mode...`
    

## 6. Implementation Phases

### Phase 1: The Static Harness

- Setup React+Vite+Tailwind.
    
- Implement the "Alcatraz" Auth Layer (PAT input + CSP configuration).
    
- Implement the GitHub API Client (Octokit wrapper) with TanStack Query.
    

### Phase 2: The Data Overlay

- Implement `CVCLoader`: Fetches `pull_request`, gets commit range, fetches `refs/cvc/main` blobs.
    
- Implement the "Join Logic": matching Commit SHAs to Interaction IDs locally in the browser.
    

### Phase 3: The UI Polish (Linearization)

- Implement the Three-Pane Layout.
    
- Apply high-contrast, minimalist styling.
    
- Add keyboard shortcuts and Command Palette (using `cmdk` package).
    
- Add "Reverse Blame" line-linking.
    

## 7. Mockup Description (Mental Model)

- **Background:** `#080808` (Almost Black, not pure black).
    
- **Borders:** `#1C1C1C` (Subtle separation).
    
- **Text:** `#EDEDED` (Primary), `#888888` (Secondary/Metadata).
    
- **Accents:** `#5E6AD2` (Indigo/Purple for AI Thoughts), `#27A300` (Green for merged code).
    
- **Animations:** * When switching files, the diff fades out/in (`opacity: 0.8 -> 1`).
    
    - The Timeline nodes slide in from the right (`x: 20 -> 0`).
