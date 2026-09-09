import * as assert from "assert";
import type { ChildProcess } from "child_process";
import test from "node:test";
import type { LanguageClient, LanguageClientOptions, ServerOptions } from "vscode-languageclient/node";
import { VoluteLanguageClient } from "../lsp/client";

class Deferred<T> {
  readonly promise: Promise<T>;
  resolve!: (value: T) => void;
  reject!: (reason?: unknown) => void;
  constructor() { this.promise = new Promise<T>((resolve, reject) => { this.resolve = resolve; this.reject = reject; }); }
}

class FakeProcess {
  killed = false;
  killCount = 0;
  readonly stderr = { on: () => undefined };
  kill(): boolean { this.killCount += 1; this.killed = true; return true; }
}

class FakeClient {
  startCount = 0;
  stopCount = 0;
  readonly started = new Deferred<void>();
  constructor(private readonly serverOptions: ServerOptions, private readonly rejectStop = false) {}
  async start(): Promise<void> { this.startCount += 1; await (this.serverOptions as () => Promise<unknown>)(); await this.started.promise; }
  async stop(): Promise<void> { this.stopCount += 1; if (this.rejectStop) { throw new Error("Starting"); } }
  setTrace(): Promise<void> { return Promise.resolve(); }
}

function fixture(options: { binary?: Promise<string | undefined>; rejectStop?: boolean; onSpawn?: () => void } = {}) {
  const processes: FakeProcess[] = [];
  const clients: FakeClient[] = [];
  const binary = options.binary ?? Promise.resolve("/safe/cvc-lsp");
  let binaryLookupCount = 0;
  const client = new VoluteLanguageClient(
    { subscriptions: [], extensionPath: "/extension" } as never,
    { appendLine: () => undefined } as never,
    { uri: { fsPath: "/workspace" } } as never,
    {
      findServerBinary: () => { binaryLookupCount += 1; return binary; },
      spawn: () => { const process = new FakeProcess(); processes.push(process); options.onSpawn?.(); return process as unknown as ChildProcess; },
      createLanguageClient: (serverOptions: ServerOptions, _options: LanguageClientOptions) => {
        const fake = new FakeClient(serverOptions, options.rejectStop);
        clients.push(fake);
        return fake as unknown as LanguageClient;
      },
      getTrace: () => "off",
      warnVerboseTrace: () => undefined,
      terminateProcess: (process) => { if (!process.killed) { process.kill(); } },
    },
  );
  return { client, processes, clients, binaryLookupCount: () => binaryLookupCount };
}

test("cancellation before and during binary discovery never spawns", async () => {
  const before = fixture();
  await before.client.start(() => false);
  assert.strictEqual(before.processes.length, 0);
  assert.strictEqual(before.binaryLookupCount(), 0);

  const discovery = new Deferred<string | undefined>();
  const during = fixture({ binary: discovery.promise });
  let active = true;
  const starting = during.client.start(() => active);
  active = false;
  discovery.resolve("/safe/cvc-lsp");
  await starting;
  assert.strictEqual(during.processes.length, 0);
  assert.strictEqual(during.binaryLookupCount(), 1);
  assert.deepStrictEqual(during.client.getLifecycleStateForTest(), { hasActiveClient: false, hasStartingClient: false, hasActiveProcess: false, hasStartingProcess: false });
});

test("stop kills the exact starting child before a rejecting client stop completes", async () => {
  const subject = fixture({ rejectStop: true });
  const starting = subject.client.start();
  await Promise.resolve();
  assert.strictEqual(subject.processes.length, 1);
  const stopping = subject.client.stop();
  assert.strictEqual(subject.processes[0].killCount, 1);
  await stopping;
  subject.clients[0].started.resolve();
  await starting;
  assert.strictEqual(subject.clients[0].stopCount, 2);
  assert.deepStrictEqual(subject.client.getLifecycleStateForTest(), { hasActiveClient: false, hasStartingClient: false, hasActiveProcess: false, hasStartingProcess: false });
});

test("a cancelled pending start cannot publish its client or process after completion", async () => {
  const subject = fixture();
  let active = true;
  const starting = subject.client.start(() => active);
  await Promise.resolve();
  active = false;
  await subject.client.stop();
  subject.clients[0].started.resolve();
  await starting;
  assert.strictEqual(subject.clients[0].stopCount, 2);
  assert.strictEqual(subject.processes[0].killCount, 1);
  assert.strictEqual(subject.client.isRunning(), false);
});

test("overlapping restarts are coalesced and track only one replacement process", async () => {
  const replacementSpawned = new Deferred<void>();
  let spawnCount = 0;
  const subject = fixture({ onSpawn: () => { spawnCount += 1; if (spawnCount === 2) { replacementSpawned.resolve(); } } });
  const initial = subject.client.start();
  await Promise.resolve();
  subject.clients[0].started.resolve();
  await initial;
  const first = subject.client.restart();
  const second = subject.client.restart();
  await replacementSpawned.promise;
  assert.strictEqual(subject.processes.length, 2);
  assert.strictEqual(subject.processes[0].killCount, 1);
  subject.clients[1].started.resolve();
  await Promise.all([first, second]);
  assert.deepStrictEqual(subject.client.getLifecycleStateForTest(), { hasActiveClient: true, hasStartingClient: false, hasActiveProcess: true, hasStartingProcess: false });
});

test("stop/start interleaving cleans orphan candidates without killing a newer process", async () => {
  const subject = fixture();
  const oldStart = subject.client.start();
  await Promise.resolve();
  await subject.client.stop();
  const newerStart = subject.client.start();
  await Promise.resolve();
  assert.strictEqual(subject.processes.length, 2);
  subject.clients[1].started.resolve();
  await newerStart;
  subject.clients[0].started.resolve();
  await oldStart;
  assert.strictEqual(subject.processes[0].killCount, 1);
  assert.strictEqual(subject.processes[1].killCount, 0);
  await subject.client.stop(); // deactivation has the same cleanup path
  assert.strictEqual(subject.processes[1].killCount, 1);
});

test("normal start/stop requests graceful client shutdown and terminates once", async () => {
  const subject = fixture();
  const starting = subject.client.start();
  await Promise.resolve();
  subject.clients[0].started.resolve();
  await starting;
  await subject.client.stop();
  assert.strictEqual(subject.clients[0].startCount, 1);
  assert.strictEqual(subject.clients[0].stopCount, 1);
  assert.strictEqual(subject.processes[0].killCount, 1);
});
