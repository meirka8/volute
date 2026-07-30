# User Stories: CVC VS Code Extension

## 1. Personas

- **Devin (The Developer):** Uses VS Code daily. Wants to stay in flow but needs to remember _why_ he made changes.
    
- **Sarah (The Lead):** Reviews code but also codes. Uses the timeline to self-review before committing.
    

## 2. Epic: The Copilot Watcher Workflow

### Story 2.1: The Seamless Log

> As Devin, after I acknowledge local capture, I want to use GitHub Copilot Chat in this workspace normally while CVC records available chat data privately, so I do not have to switch tools or copy-paste logs.

- **Scenario:** Devin is stuck on a Rust borrow checker error.
    
- **Action:** He opens a new GitHub Copilot Chat and asks, `How do I fix this lifetime issue?`.
    
- **System:** The watcher, already gated by acknowledgement and mapped to this exact workspace only, sees the Copilot session update and records available prompts, responses, exposed reasoning, tool metadata, and context patches privately.
    
- **Verification:** Devin glances at the Volute timeline panel and sees a new pending thought appear with his question.
    

### Story 2.2: The Copilot-Specific Watcher

> As Devin, I want the VS Code workflow to work specifically with GitHub Copilot chat, so I know exactly which conversations Volute is watching.

- **Scenario:** Devin uses GitHub Copilot in VS Code and also has other agent tools installed.
    
- **Action:** He continues working in GitHub Copilot Chat as normal.
    
- **System:** The watcher follows GitHub Copilot's local chat storage format only after acknowledgement and only when its workspace metadata exactly matches the open workspace. It does not inspect a recent or neighboring workspace's storage.

### Story 2.3: Consent revocation stops observation

> As Devin, when local capture consent is no longer available, I want the watcher to stop immediately so it cannot continue reading chat storage on stale permission.

- **System:** On a privacy-status refresh that reports revoked, unavailable, or unconfirmed passive-capture consent, CVC stops and discards the active watcher before prompting. It starts a new watcher only after a later positive acknowledgement status.
    

## 3. Epic: The Cognitive Timeline (Side Panel)

### Story 3.1: The Pre-Commit Review ("Am I Tracking?")

> As Sarah, before I run `git commit`, I want to see which captured interactions are currently "Floating", so I can review what may be eligible for this unit of work.

- **Scenario:** Sarah finishes a complex feature. She opens the CVC Panel.
    
- **Observation:** She sees 5 "Pending Thoughts" under the top section.
    
- **Action:** She realizes one thought was a random question about lunch (unrelated). She right-clicks and selects "Delete Thought" to exclude it from the permanent record.
    
- **Result:** She commits the code. Interactions that satisfy the conservative time/file-context linking policy may be bound; unrelated, stale, or disjoint interactions remain floating.

### Story 3.4: The PR Shadow Timeline

> As Sarah, after explicitly sharing and publishing selected interactions for a branch, I want to see the available captured context beside the diff without implying that it contains hidden or complete model reasoning.

- **Scenario:** Devin pushes his branch and opens a PR.

- **Action:** Sarah opens the hosted reviewer.

- **Result:** She sees the shadow timeline beside the diff and can follow why the code changed, not just what changed.
    

### Story 3.2: The "After Lunch" Context Refresh

> As Devin, returning from a break, I want to review the prompts and context CVC captured 2 hours ago, so I can recover useful task context.

- **Scenario:** Devin sits down after lunch. He forgot exactly where he left off.
    
- **Action:** He looks at the "Pending Thoughts" list. He sees: _"Prompt: Try using a mutex here instead of a channel."_
    
- **Result:** He immediately remembers the architectural pivot he was attempting.
    

### Story 3.3: The Historical Lookup

> As Sarah, I see a weird function committed yesterday. I want to see the conversation that generated it without leaving my editor.

- **Scenario:** Sarah is browsing `auth.ts`.
    
- **Action:** She looks at the "History" section of the CVC Panel. She expands the commit `feat: add auth`.
    
- **Result:** She clicks the interaction node. A side panel opens showing the available captured fields. If the stored response contains an AI warning about a specific edge case, it can help explain the code structure.
