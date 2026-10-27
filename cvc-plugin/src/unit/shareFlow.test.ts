import * as assert from "assert";
import test from "node:test";
import {
  chooseShareFlow,
  cliHasConversationsView,
  conversationsViewCanShare,
  resolveShareFlow,
} from "../setup/shareFlow";

/** Platforms where the CLI's view can share (it discards type-ahead before a typed phrase). */
const VIEW_PLATFORMS: NodeJS.Platform[] = ["linux", "darwin"];

/** Windows, where the view only browses, and the platforms VS Code does not run on. */
const BOX_PLATFORMS: NodeJS.Platform[] = [
  "win32",
  "freebsd",
  "openbsd",
  "netbsd",
  "sunos",
  "aix",
  "android",
  "cygwin",
  "haiku",
];

/** CLI versions that have the view: 0.7.1 is the first release with it. */
const VERSIONS_WITH_VIEW = ["0.7.1", "0.7.1+build.5", "0.7.2", "0.7.10", "0.8.0", "0.8.0-rc.1", "0.10.0", "1.0.0"];

/** CLI versions that predate the view, including a prerelease of the first release that has it. */
const VERSIONS_BEFORE_VIEW = ["0.7.0", "0.6.9", "0.6.0", "0.5.0", "0.4.2", "0.0.1", "0.7.1-rc.1"];

/** What the CLI's version is when it could not be read, or was not a version. */
const UNKNOWN_VERSIONS: Array<string | undefined> = [
  undefined,
  "",
  "unknown",
  "latest",
  "v0.7.1",
  "0.7",
  "0.7.1.1",
  "cvc 0.7.1",
  " 0.7.1",
  "0.7.1\n",
];

const ALL_VERSIONS = [...VERSIONS_WITH_VIEW, ...VERSIONS_BEFORE_VIEW, ...UNKNOWN_VERSIONS];

test("Linux and macOS open the conversations view for a CLI that has it", () => {
  for (const platform of VIEW_PLATFORMS) {
    for (const version of VERSIONS_WITH_VIEW) {
      assert.strictEqual(chooseShareFlow(platform, version), "conversations-view", `${platform} ${version}`);
    }
  }
});

test("Linux and macOS keep the input boxes for a CLI that predates the view", () => {
  for (const platform of VIEW_PLATFORMS) {
    for (const version of VERSIONS_BEFORE_VIEW) {
      assert.strictEqual(chooseShareFlow(platform, version), "input-boxes", `${platform} ${version}`);
    }
  }
});

test("Linux and macOS keep the input boxes when the CLI's version is unknown", () => {
  for (const platform of VIEW_PLATFORMS) {
    for (const version of UNKNOWN_VERSIONS) {
      assert.strictEqual(chooseShareFlow(platform, version), "input-boxes", `${platform} ${JSON.stringify(version)}`);
    }
  }
});

test("Windows keeps the input boxes whatever the CLI reports", () => {
  for (const version of ALL_VERSIONS) {
    assert.strictEqual(chooseShareFlow("win32", version), "input-boxes", JSON.stringify(version));
  }
});

test("a platform that is neither Linux, macOS nor Windows keeps the input boxes", () => {
  for (const platform of BOX_PLATFORMS) {
    for (const version of ALL_VERSIONS) {
      assert.strictEqual(chooseShareFlow(platform, version), "input-boxes", `${platform} ${JSON.stringify(version)}`);
    }
  }
});

test("only Linux and macOS can share in the view", () => {
  for (const platform of VIEW_PLATFORMS) {
    assert.strictEqual(conversationsViewCanShare(platform), true, platform);
  }
  for (const platform of BOX_PLATFORMS) {
    assert.strictEqual(conversationsViewCanShare(platform), false, platform);
  }
});

test("0.7.1 is the first CLI release with the view", () => {
  assert.strictEqual(cliHasConversationsView("0.7.0"), false);
  assert.strictEqual(cliHasConversationsView("0.7.1-rc.1"), false);
  assert.strictEqual(cliHasConversationsView("0.7.1"), true);
  assert.strictEqual(cliHasConversationsView(undefined), false);
});

/** A reader of the CLI's version that records what it was asked for. */
function recordingReader(version: string | undefined): {
  read: (cvcBinary: string) => Promise<string | undefined>;
  asked: string[];
} {
  const asked: string[] = [];
  return {
    read: async (cvcBinary) => {
      asked.push(cvcBinary);
      return version;
    },
    asked,
  };
}

test("reads the version of the resolved CLI and chooses the view for a new one", async () => {
  const { read, asked } = recordingReader("0.7.1");
  assert.deepStrictEqual(await resolveShareFlow("linux", "/home/dev/.cvc/bin/cvc", read), {
    flow: "conversations-view",
    cliVersion: "0.7.1",
  });
  assert.deepStrictEqual(asked, ["/home/dev/.cvc/bin/cvc"]);
  assert.strictEqual((await resolveShareFlow("darwin", "/Users/dev/.cvc/bin/cvc", read)).flow, "conversations-view");
});

test("keeps the input boxes, and reports the version, for a CLI that predates the view", async () => {
  const { read, asked } = recordingReader("0.7.0");
  assert.deepStrictEqual(await resolveShareFlow("linux", "/home/dev/.cvc/bin/cvc", read), {
    flow: "input-boxes",
    cliVersion: "0.7.0",
  });
  assert.strictEqual(asked.length, 1);
});

test("keeps the input boxes when the CLI's version cannot be read", async () => {
  const { read, asked } = recordingReader(undefined);
  assert.deepStrictEqual(await resolveShareFlow("linux", "/home/dev/.cvc/bin/cvc", read), {
    flow: "input-boxes",
    cliVersion: undefined,
  });
  assert.strictEqual(asked.length, 1);
});

test("never starts the version probe where the answer cannot change", async () => {
  for (const platform of BOX_PLATFORMS) {
    const { read, asked } = recordingReader("0.7.1");
    assert.deepStrictEqual(await resolveShareFlow(platform, "C:\\Users\\dev\\.cvc\\bin\\cvc.exe", read), {
      flow: "input-boxes",
      cliVersion: undefined,
    });
    assert.deepStrictEqual(asked, [], platform);
  }
  for (const platform of VIEW_PLATFORMS) {
    const { read, asked } = recordingReader("0.7.1");
    assert.deepStrictEqual(await resolveShareFlow(platform, undefined, read), {
      flow: "input-boxes",
      cliVersion: undefined,
    });
    assert.deepStrictEqual(asked, [], `${platform} without a resolved CLI`);
  }
});
