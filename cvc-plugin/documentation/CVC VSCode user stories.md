# User Stories: CVC VS Code Extension

## 1. Personas

- **Devin (The Developer):** Uses VS Code daily. Wants to stay in flow but needs to remember _why_ he made changes.
    
- **Sarah (The Lead):** Reviews code but also codes. Uses the timeline to self-review before committing.
    

## 2. Epic: The Native Chat Workflow

### Story 2.1: The Seamless Log

> As Devin, I want to ask the AI for help using my standard Chat UI, but have it automatically recorded, so I don't have to switch tools or copy-paste logs.

- **Scenario:** Devin is stuck on a Rust borrow checker error.
    
- **Action:** He opens the Chat Sidebar and types `@cvc How do I fix this lifetime issue?`.
    
- **System:** The extension delegates to Copilot GPT-4. The answer appears instantly.
    
- **Verification:** Devin glances at the "Cognitive Timeline" panel and sees a new "Pending Thought" appear with his question.
    

### Story 2.2: The Context Carrier

> As Devin, I want the `@cvc` participant to see the file I'm looking at, so I don't have to manually copy-paste code context.

- **Scenario:** Devin has `main.rs` open.
    
- **Action:** He asks `@cvc refactor this function`.
    
- **System:** The extension detects the active editor and passes the file reference to the language model and the CVC logger.
    

## 3. Epic: The Cognitive Timeline (Side Panel)

### Story 3.1: The Pre-Commit Review ("Am I Tracking?")

> As Sarah, before I run `git commit`, I want to see what thoughts are currently "Floating", so I can be sure my reasoning is attached to this unit of work.

- **Scenario:** Sarah finishes a complex feature. She opens the CVC Panel.
    
- **Observation:** She sees 5 "Pending Thoughts" under the top section.
    
- **Action:** She realizes one thought was a random question about lunch (unrelated). She right-clicks and selects "Delete Thought" to exclude it from the permanent record.
    
- **Result:** She commits the code, and the remaining 4 thoughts are automatically bound.
    

### Story 3.2: The "After Lunch" Context Refresh

> As Devin, returning from a break, I want to see what I was thinking about 2 hours ago, so I can resume my mental state immediately.

- **Scenario:** Devin sits down after lunch. He forgot exactly where he left off.
    
- **Action:** He looks at the "Pending Thoughts" list. He sees: _"Prompt: Try using a mutex here instead of a channel."_
    
- **Result:** He immediately remembers the architectural pivot he was attempting.
    

### Story 3.3: The Historical Lookup

> As Sarah, I see a weird function committed yesterday. I want to see the conversation that generated it without leaving my editor.

- **Scenario:** Sarah is browsing `auth.ts`.
    
- **Action:** She looks at the "History" section of the CVC Panel. She expands the commit `feat: add auth`.
    
- **Result:** She clicks the thought node. A side panel opens showing the full chat transcript. She sees the AI warning about a specific edge case, which explains the code structure.