import * as assert from "assert";
import {
  createInstallCommand,
  createCvcInitExecution,
  getInstallerUrls,
} from "../setup/dependencyManager";

suite("dependency setup security", () => {
  test("uses immutable version-tagged installer URLs", () => {
    assert.deepStrictEqual(getInstallerUrls("0.4.1"), {
      sh: "https://raw.githubusercontent.com/meirka8/volute/v0.4.1/install.sh",
      ps1: "https://raw.githubusercontent.com/meirka8/volute/v0.4.1/install.ps1",
      releaseTag: "v0.4.1",
    });
  });

  test("refuses malformed versions before constructing installer URLs", () => {
    for (const version of [undefined, "", "main", "0.4.1/../../main", "0.4.1?ref=main"]) {
      assert.strictEqual(getInstallerUrls(version), undefined);
    }
  });

  test("keeps a metacharacter-containing binary path out of shell text", () => {
    const configuredPath = "/tmp/cvc; touch should-not-run #";
    const execution = createCvcInitExecution(configuredPath, "/workspace/repository");

    assert.strictEqual(execution.process, configuredPath);
    assert.deepStrictEqual(execution.args, ["init"]);
    assert.deepStrictEqual(execution.options, { cwd: "/workspace/repository" });
  });

  test("uses a unique temporary PowerShell installer with strict cleanup", () => {
    const urls = getInstallerUrls("0.4.1");
    assert.ok(urls);

    const command = createInstallCommand("win32", urls);
    assert.match(command, /\$ErrorActionPreference = 'Stop'/);
    assert.match(command, /\$env:CVC_RELEASE_VERSION = 'v0\.4\.1'/);
    assert.match(command, /New-TemporaryFile/);
    assert.match(command, /try \{/);
    assert.match(command, /finally \{ Remove-Item -LiteralPath \$tempFile/);
    assert.doesNotMatch(command, /cvc-install\.ps1/);
  });

  test("pins the Unix installer to the extension release", () => {
    const urls = getInstallerUrls("0.4.1");
    assert.ok(urls);

    assert.strictEqual(
      createInstallCommand("unix", urls),
      "curl -fsSL 'https://raw.githubusercontent.com/meirka8/volute/v0.4.1/install.sh' | CVC_RELEASE_VERSION='v0.4.1' sh",
    );
  });
});
