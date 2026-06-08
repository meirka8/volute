# User Stories: CVC Reviewer Webapp

## 1. Personas

|   |   |   |   |
|---|---|---|---|
|**Persona**|**Role**|**Primary Motivation**|**Key Frustration**|
|**Alice (The Tech Lead)**|Senior Engineer|**Velocity & Quality.** Wants to approve PRs quickly but catches subtle bugs that tests miss.|"Why did you refactor this perfectly good function?" (Context switching to Ask/Wait for reply).|
|**Bob (The SecOps)**|Security Architect|**Data Sovereignty.** Ensures no proprietary IP leaks to third-party SaaS.|"I can't let you use that fancy AI tool because it stores our source code."|
|**Charlie (The Junior)**|New Hire|**Learning.** Wants to understand the system architecture and decision-making process.|"I see _what_ the code does, but I don't understand the business logic behind it."|

## 2. Epic: Setup & Authentication (The "Alcatraz" Flow)

**Goal:** Establish trust immediately. The user must feel confident that their credentials and code remain local/direct.

### Story 2.1: The Direct GitHub Login (Bob)

> As a security-conscious reviewer, I want to authenticate directly with GitHub without routing my credentials through a CVC backend, so that I can better understand where my repository access is being used.

- **Scenario:** Bob opens `reviewer.cvc.dev`. He sees a "Paste PAT" input field and a prominent "Client-Side Only" badge.
    
- **Action:** He disconnects his WiFi and pastes a fake token. The UI attempts a `fetch` and fails locally. He reconnects and pastes his real PAT.
    
- **Success:** The dashboard loads promptly. He checks the Network tab and verifies the current implementation is making GitHub API calls to `api.github.com`.
    

### Story 2.2: The Local Proxy Bypass (Enterprise Alice)

> As a developer at a company that blocks browser extensions and pasting tokens, I want to run the tool locally so I can use my machine's existing SSH/Git credentials.

- **Scenario:** Alice cannot paste a PAT due to company policy.
    
- **Action:** She types `cvc ui` in her terminal. The browser opens `localhost:3000`.
    
- **Success:** The app detects the local proxy and skips the login screen entirely, displaying her current repo's PRs immediately.
    

## 3. Epic: The Review Workflow (The "Linear" Flow)

**Goal:** Enable a high-velocity review state where "Intent" and "Artifact" are visible simultaneously.

### Story 3.1: The "Why" Check (Alice)

> As a reviewer looking at a confusing refactor, I want to see the prompt that generated it, so I can determine if the implementation matches the intent without asking the author.

- **Scenario:** Alice sees a complex Regex change in `input_validation.ts`.
    
- **Action:** She glances at the **Right Pane (Shadow Timeline)** aligned with that file. She sees a node: _"User: Make the email validation RFC 5322 compliant but ignore comments."_
    
- **Result:** She quickly understands the complexity is intentional, not accidental. She approves the file.
    

### Story 3.2: The "Reverse Blame" Discovery (Charlie)

> As a junior engineer reading code, I want to click on a specific line and see the conversation that created it, so I can learn the reasoning behind specific logic.

- **Scenario:** Charlie is reviewing a PR and sees a weird `if (!user) return;` guard clause that seems redundant.
    
- **Action:** He clicks the line number in the diff.
    
- **Reaction:** The Right Pane auto-scrolls and highlights the exact interaction where the Agent said: _"We need this guard clause because the `auth` middleware might run after this hook in edge case X."_
    
- **Success:** Charlie learns about the race condition without pinging the senior dev.
    

### Story 3.3: Keyboard Navigation (Power User Alice)

> As a power user, I want to navigate the entire PR using only my keyboard, so I can maintain my "flow state" similar to using Vim or Linear.

- **Scenario:** Alice has 15 files to review.
    
- **Action:**
    
    - She presses `j` to move down the file list.
        
    - She presses `Space` to mark a file as "Viewed."
        
    - She presses `x` to expand the AI's "Reasoning" block for the current file.
        
    - She presses `Cmd+K` -> "Approve" to submit the review.
        
- **Success:** She reviews the entire PR in 5 minutes without touching the mouse.
    

## 4. Epic: Verification & Confidence

**Goal:** Verify that the AI's output wasn't blindly accepted by the developer.

### Story 4.1: The "Blind Paste" Detection (Alice)

> As a reviewer, I want to know if the author accepted the AI's code without modification, so I can scrutinize it more heavily.

- **Scenario:** Alice is reviewing a large generic function.
    
- **Action:** She looks at the Shadow Timeline. She sees:
    
    1. _User Prompt:_ "Write a function to parse CSV."
        
    2. _Agent Response:_ [Code Block]
        
    3. _Git Commit:_ Matches the Agent Response exactly (100% similarity).
        
- **Result:** The UI flags this with a subtle "Unmodified Generation" indicator. Alice decides to double-check the edge cases because the author likely just copy-pasted it.
    

### Story 4.2: The Iteration History (Bob)

> As a reviewer, I want to see the _failed_ attempts before the final commit, so I know what approaches were already ruled out.

- **Scenario:** The code uses a specific library `LibA`. Bob wonders why they didn't use `LibB`.
    
- **Action:** He scrolls up the Shadow Timeline. He sees an earlier Interaction Node where the Agent tried `LibB`, failed with a compilation error, and the user prompted: _"LibB is broken, try LibA instead."_
    
- **Result:** Bob saves time by not writing a comment suggesting `LibB`.
