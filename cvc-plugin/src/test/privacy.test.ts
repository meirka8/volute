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
