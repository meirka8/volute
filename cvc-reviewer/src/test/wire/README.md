# Wire fixtures

Interaction blobs exactly as the Claude Code harness of the shipped cvc 0.7.0 binary wrote them. `v2.1.260-basic/` was ingested from the repo's public synthetic transcript `cvc-core/tests/fixtures/claude-code/v2.1.260/basic.jsonl`; `edge-cases/` from one synthetic transcript written for these tests (redaction markers, truncation, a server-side tool, unanswered tool calls).
The only edit is that local paths were replaced by `/work/repo`; files are named `<fixture>/<interaction id>.json` and are kept byte for byte, so do not reformat them.
They hold no real data: the only address is `fixture@example.invalid`, and redacted values appear only as `[CVC_REDACTED:v1:...]` markers.
`../wireNodes.test.tsx` loads every file in this directory, so a new file needs an entry there.
