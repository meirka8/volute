# Project Design: CVC Reviewer Webapp

## 1. Executive Summary

The **CVC Reviewer** is a visualization layer for Pull Requests that overlays available CVC interaction data (prompts, context, optional exposed reasoning, and responses) atop standard code diffs. It renders only reasoning that a source supplied and exposed; hidden model reasoning is not available.

**Primary Mandates:**

1. **Local PAT Security:** In local PAT mode, repository API requests go from the browser directly to GitHub and no CVC-hosted API is required. Hosted mode has a different token flow and must not be described with local-mode guarantees.
    
2. **Linear-Grade UX:** A "Tool-First" design philosophy focusing on keyboard centricity, sub-100ms latency perception, and information density without clutter.
    

## 2. The UX Doctrine: "Tool-First" (The Linear Method)

We reject the "Enterprise Dashboard" aesthetic (Jira, ServiceNow) in favor of **"Utilitarian Luxury."** The interface is designed for power users who spend hours in the tool.

### 2.1 Core UX Principles

- **The 100ms Target:** Local UI interactions should target a sub-100ms perceived response where practical. Network-backed operations can take longer and must show honest loading or stale-data state.
    
- **Keyboard is King:** The mouse is a fallback.
    
    - `j` / `k`: Navigate commits/thoughts.
        
    - `x`: Expand/Collapse reasoning.
        
    - `Cmd+K`: Command Palette for all actions.
        
- **Contextual Density:**
    
    - _Scan Mode:_ By default, the timeline shows only "Intent" (User Prompts).
        
    - _Focus Mode:_ Hovering or selecting a node reveals available integration-exposed or explicitly supplied reasoning and context details.
        
- **High-Contrast Typography:** Use a rigorous type scale. Code is `Monospace`; UI is `Inter`/`San Francisco`. Interactive elements are distinct from read-only text not by color alone, but by position and weight.
    

### 2.2 The "Anti-Bloat" Rules

1. **No Dropdowns:** If there are < 5 options, use a toggle group or visible radio list.
    
2. **No Wizards:** Settings and configurations happen in-place or via the Command Palette.
    
3. **No "Save" Buttons:** All state is persisted immediately to local storage or the URL.
    

## 3. Security Architecture: Local PAT Mode

The default self-contained mode runs the reviewer in the browser without requiring a CVC-hosted API. This is a deployment boundary, not an absolute privacy guarantee: the app makes network requests to GitHub, stores the PAT in browser session storage, and runs within the user's browser, extensions, developer tools, and hosting environment. Optional hosted mode must be evaluated separately.

### 3.1 Data Flow

- **Local PAT mode:** `User Browser (PAT in session storage) -> api.github.com` for repository requests.

- **Static assets:** HTML, JavaScript, CSS, fonts, and images can be served by origins allowed by the deployment's Content Security Policy.

- **Hosted mode:** A deployment that explicitly enables hosted mode has additional service and credential boundaries; local-PAT statements do not apply to it.
        

### 3.2 Authentication Strategy

- **Current local option: Personal Access Tokens (PAT):**
    
    - _Mechanism:_ User generates a granular PAT (read-only) and pastes it.
        
    - _Trade-off:_ No CVC-hosted API is required, but security still depends on GitHub, the hosting origin, dependencies, the browser profile, extensions, and the user's device.
        
    - _Cons:_ High friction.
        
- **Other modes:** Hosted or local-proxy designs have separate threat models and should be documented only against their implemented deployment behavior. They do not inherit local PAT mode's direct-request boundary.
        

### 3.3 Content Security Policy (CSP)

The current document CSP restricts resource and connection origins, including GitHub API access and mode-specific self/local/configured endpoints. CSP is defense in depth against some injection and exfiltration paths; it is not proof that a browser, extension, dependency, allowed origin, or compromised hosting environment cannot expose data.

```
default-src 'self';
connect-src 'self' https://api.github.com http://localhost:3000 <configured-platform-origin>;
script-src 'self';
style-src 'self' 'unsafe-inline';

```

The concrete configured platform origin depends on the deployment mode.

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
|Status Icons (Reviewed/New)|Line-number linking|Prompts and available supplied/exposed reasoning|

### 5.2 The "Shadow Timeline" (Right Pane)

This is the heart of CVC Reviewer. It visualizes the `refs/cvc/main` data linked to the currently visible code.

- **Node Representation:**
    
    - _User Node:_ Avatar + Prompt Text (Truncated).
        
    - _Model Node:_ Icon + Response Summary.
        
    - _Optional Data:_ Reasoning is shown only when the source supplied, exposed, and stored it; hidden model reasoning is not assumed.
        
- **Reverse Blame (The "Why" Click):**
    
    - When a user clicks a line of code in the Middle Pane (Diff), the Right Pane can auto-scroll to interactions linked to the associated commit. Commit-level linkage does not prove that a specific interaction generated that exact line.
        
    - _Visual Cue:_ A connecting bezier curve line (using SVG overlay) momentarily draws between the code line and the thought node.
        

### 5.3 Command Palette (`Cmd+K`)

The primary controller.

- `> Go to file...`
    
- `> View reasoning...`
    
- `> Switch to local mode...`
    
- `> Toggle dark mode...`
    

## 6. Implementation Phases

### 5.4 FORMAT5 evidence, suppression, and browser retention

The reviewer supports sync formats v1–v5. For v5 it validates bounded, canonical
`events/` derivation records and `ranges/` `RangeEvidence`, including the
`cvc.changeset/v1` identity and exact source/range closure, before it joins an event to a
validated node and `by-commit` pointer. This is wire-format validation only: labels such as
**Publisher-asserted Git rewrite** and **Publisher-asserted squash equivalence** do not mean
the browser independently verified local Git objects, trust observations, or authorization.

For v4+ it validates and loads the `tombstones/` tree before nodes; a valid tombstone
suppresses its target regardless of whether that target appears in a legacy flat path or a
sharded `nodes/` path, and suppresses that interaction's v5 derivation/range closure. It
evicts the target's React Query node-cache key on observation. Invalid or truncated tombstone
data is an error, not permission to display a possibly suppressed interaction.

This is UI/cache suppression only. A tombstone is not physical Git-object deletion, and
cache eviction cannot clear GitHub responses, browser session storage, developer tools,
downloads, clones, forks, reflogs, caches, or backups. In local PAT mode, the PAT is held
in the browser session, including session storage, and no CVC-hosted token store is
required. Hosted deployments have a different boundary. Users remain responsible for
their browser environment.

Immutable cognitive query results may remain in the reviewer's IndexedDB query cache for
up to 30 days. Tombstone observation removes affected entries from both in-memory and
persisted cache projections. Logout and PAT/account replacement clear all reviewer React
Query and IndexedDB caches. A malformed, missing, or truncated tombstone namespace—or a
malformed format marker—is fail-closed: the reviewer purges its cognitive cache and does
not render a possibly suppressed timeline.

### Phase 1: The Static Harness

- Setup React+Vite+Tailwind.
    
- Implement the "Alcatraz" Auth Layer (PAT input + CSP configuration).
    
- Implement the GitHub API Client (Octokit wrapper) with TanStack Query.
    

### Phase 2: The Data Overlay

- Implement `CVCLoader`: Fetches `pull_request`, gets commit range in bounded 100-commit pages (maximum 100 pages), includes the reported merged PR SHA, then fetches `refs/cvc/main` blobs. Invalid pagination or merged SHA fails the cognitive overlay closed.
    
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
