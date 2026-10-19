import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import {
  isExactWorkspaceStorage,
  isSafeInteractionId,
  mayStartPassiveWatcher,
  PassiveWatcherGate,
} from "../privacy";

const acknowledged = {
  captureAcknowledged: true,
  captureNoticeVersion: 1,
  passiveCaptureAllowed: true,
  privateByDefault: true,
  privateDefaultStatement: "",
  sharingSummary: "",
  autoPushEnabled: false,
};

suite("privacy capture boundary", () => {
  test("does not create a watcher before acknowledgement", () => {
    assert.strictEqual(mayStartPassiveWatcher(undefined, false), false);
    assert.strictEqual(
      mayStartPassiveWatcher({
        captureAcknowledged: false,
        captureNoticeVersion: 1,
        passiveCaptureAllowed: false,
        privateByDefault: true,
        privateDefaultStatement: "",
        sharingSummary: "",
        autoPushEnabled: false,
      }, false),
      false,
    );
  });

  test("opening the acknowledgement terminal does not imply acknowledgement", () => {
    // The terminal command has no state transition; only a newly fetched LSP
    // policy result with passiveCaptureAllowed permits storage observation.
    assert.strictEqual(mayStartPassiveWatcher(undefined, false), false);
  });

  test("a refresh may start exactly one watcher after acknowledgement", () => {
    assert.strictEqual(mayStartPassiveWatcher(acknowledged, false), true);
    assert.strictEqual(mayStartPassiveWatcher(acknowledged, true), false);
  });

  test("revokes an active watcher immediately and permits a fresh watcher after re-acknowledgement", () => {
    class FakeWatcher {
      stopped = false;
      reads = 0;

      stop(): void {
        this.stopped = true;
      }

      readSessionContent(): void {
        if (!this.stopped) {
          this.reads += 1;
        }
      }
    }

    const gate = new PassiveWatcherGate<FakeWatcher>();
    const first = gate.reconcile(acknowledged, () => new FakeWatcher());
    assert.ok(first);
    first.readSessionContent();
    assert.strictEqual(first.reads, 1);

    const revoked = { ...acknowledged, captureAcknowledged: false, passiveCaptureAllowed: false };
    assert.strictEqual(gate.reconcile(revoked, () => new FakeWatcher()), undefined);
    assert.strictEqual(first.stopped, true);
    first.readSessionContent();
    assert.strictEqual(first.reads, 1, "a stopped watcher must not read further session content");

    const second = gate.reconcile(acknowledged, () => new FakeWatcher());
    assert.ok(second);
    assert.notStrictEqual(second, first);
  });

  test("uses only an exact workspace storage mapping", () => {
    assert.strictEqual(isExactWorkspaceStorage({ folder: "file:///workspace/a" }, "file:///workspace/a"), true);
    assert.strictEqual(isExactWorkspaceStorage({ workspace: "file:///workspace/a" }, "file:///workspace/a"), true);
    assert.strictEqual(isExactWorkspaceStorage({ folder: "file:///workspace/b" }, "file:///workspace/a"), false);
    assert.strictEqual(isExactWorkspaceStorage({}, "file:///workspace/a"), false);
  });

  test("normal-log identifiers must be validated UUIDs", () => {
    assert.strictEqual(isSafeInteractionId("550e8400-e29b-41d4-a716-446655440000"), true);
    assert.strictEqual(isSafeInteractionId("prompt content must never be logged"), false);
    assert.strictEqual(isSafeInteractionId("../../workspace/session.json"), false);
  });

  test("does not ship explicit participant or raw chat log templates", () => {
    const extensionBundle = fs.readFileSync(
      path.resolve(__dirname, "..", "extension.js"),
      "utf-8",
    );
    assert.doesNotMatch(extensionBundle, /User prompt:/);
    assert.doesNotMatch(extensionBundle, /prompt\.substring/);
    assert.doesNotMatch(extensionBundle, /File change detected: \$\{/);
    assert.doesNotMatch(extensionBundle, /Chat Participant registered/);
  });
});

// Real ids written by the shipped Claude Code harness: RFC 9562 version 8, with the RFC variant bits.
// Taken from cvc-reviewer/src/test/wire/v2.1.260-basic/; between them they carry all four variant digits.
const HARNESS_IDS = [
  "daa5ceb6-7e4d-8a1d-8632-c6b36b563250", // variant 8
  "7dbdb307-1fb2-8a87-97b5-69be2b9b1f74", // variant 9
  "916ca1ec-d59e-89f5-a596-7a1d355212c9", // variant a
  "3ec4e81b-3ffc-8d60-b160-5c0b3a2407db", // variant b
];

/** An id of the canonical shape with its version and variant digits chosen (each is one hex digit). */
function idWith(version: string, variant: string): string {
  return `123e4567-e89b-${version}2d3-${variant}456-426614174000`;
}

suite("interaction ids in the normal log", () => {
  test("accepts the id of a Claude Code harness node, which is RFC 9562 version 8", () => {
    for (const id of HARNESS_IDS) {
      assert.strictEqual(isSafeInteractionId(id), true, id);
    }
  });

  test("accepts every version RFC 9562 defines, 1 to 8", () => {
    for (const version of ["1", "2", "3", "4", "5", "6", "7", "8"]) {
      assert.strictEqual(isSafeInteractionId(idWith(version, "a")), true, `version ${version}`);
    }
  });

  test("refuses the digits RFC 9562 gives no version to: 0 and 9 to f", () => {
    for (const version of ["0", "9", "a", "b", "c", "d", "e", "f"]) {
      for (const variant of ["8", "9", "a", "b"]) {
        const id = idWith(version, variant);
        assert.strictEqual(isSafeInteractionId(id), false, id);
      }
    }
  });

  test("refuses a variant other than the RFC variant", () => {
    for (const variant of ["0", "1", "2", "3", "4", "5", "6", "7", "c", "d", "e", "f"]) {
      for (const version of ["4", "8"]) {
        const id = idWith(version, variant);
        assert.strictEqual(isSafeInteractionId(id), false, id);
      }
    }
  });

  test("accepts a valid id in either case, as before", () => {
    assert.strictEqual(isSafeInteractionId("550E8400-E29B-41D4-A716-446655440000"), true);
    assert.strictEqual(isSafeInteractionId(HARNESS_IDS[0].toUpperCase()), true);
  });

  test("still refuses anything that is not exactly one UUID", () => {
    const id = HARNESS_IDS[0];
    for (const bad of [
      "00000000-0000-0000-0000-000000000000",
      "ffffffff-ffff-ffff-ffff-ffffffffffff",
      id.replaceAll("-", ""),
      `{${id}}`,
      `urn:uuid:${id}`,
      ` ${id}`,
      `${id} `,
      `${id}\n`, // a trailing newline could forge a second line in the log
      `${id}\nUser prompt: secret`,
      id.slice(0, -1),
      `${id}0`,
      "",
    ]) {
      assert.strictEqual(isSafeInteractionId(bad), false, JSON.stringify(bad));
    }
    for (const notText of [undefined, null, 42, true, {}, [id], { toString: () => id }]) {
      assert.strictEqual(isSafeInteractionId(notText), false, typeof notText);
    }
  });
});
