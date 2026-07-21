import type { GithubClient } from "../api/github";
import type { ArtifactLinkData } from "../types/cvc";

/** These limits intentionally mirror cvc-core/src/sync.rs. A partial v5 namespace is unsafe. */
const MAX_BLOB = 4 * 1024 * 1024;
const MAX_TOTAL = 128 * 1024 * 1024;
const MAX_EVENTS = 20_000;
const MAX_RANGES = 10_000;
const MAX_RANGE_COMMITS = 2048;
const HEX40 = /^[0-9a-f]{40}$/;
const HEX64 = /^[0-9a-f]{64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export interface DerivationEvent {
  event_id: string; interaction_id: string; target_commit: string;
  relation: "generated" | "temporal" | "verified" | "rewrite_exact" | "squash_exact";
  evidence: { version: 1; kind: "locally_observed" | "imported_legacy" | "remote_assertion" };
  origin: "local_hook" | "local_linker" | "remote_assertion" | "legacy_import";
  source_event_ids: string[]; old_oid?: string | null; new_oid?: string | null;
  range_id?: string | null; linked_by?: string | null;
}
export interface RangeEvidence { range_id: string; format: "cvc.range-evidence/v1"; version: 1; repository_identity: string; object_format: "sha1"; base_oid: string; tip_oid: string; base_tree_oid: string; result_tree_oid: string; commits: { commit_oid: string }[]; changeset_algorithm: "cvc.changeset/v1"; changeset_digest: string }

type TreeEntry = { path?: string; type?: string; sha?: string; size?: number };
const encoder = new TextEncoder();
const bytes = (...parts: (Uint8Array | string)[]) => {
  const values = parts.map((x) => typeof x === "string" ? encoder.encode(x) : x);
  const result = new Uint8Array(values.reduce((n, x) => n + x.length, 0)); let offset = 0;
  for (const value of values) { result.set(value, offset); offset += value.length; }
  return result;
};
const u64 = (n: number) => { const b = new Uint8Array(8); let x = BigInt(n); for (let i = 7; i >= 0; i--) { b[i] = Number(x & 255n); x >>= 8n; } return b; };
const tagged = (tag: number, value: string) => bytes(new Uint8Array([tag]), u64(encoder.encode(value).length), value);
const optional = (tag: number, value: string | null | undefined) => value == null ? new Uint8Array([tag, 0]) : bytes(new Uint8Array([tag, 1]), tagged(tag, value));
const sha256 = async (value: Uint8Array) => Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", value as unknown as BufferSource))).map((x) => x.toString(16).padStart(2, "0")).join("");

export async function canonicalEventId(e: Omit<DerivationEvent, "event_id">): Promise<string> {
  const sources = [...new Set(e.source_event_ids)].sort();
  return sha256(bytes("cvc.derivation-event/canonical/v2\0", tagged(1, e.interaction_id), tagged(2, e.target_commit), tagged(3, e.relation), tagged(4, String(e.evidence.version)), tagged(5, e.evidence.kind), tagged(6, e.origin), new Uint8Array([7]), u64(sources.length), ...sources.map((id) => tagged(8, id)), optional(9, e.old_oid), optional(10, e.new_oid), optional(11, e.range_id), optional(12, e.linked_by)));
}
export async function canonicalRangeId(r: Omit<RangeEvidence, "range_id">): Promise<string> {
  return sha256(bytes("cvc.range-evidence/canonical/v1\0", tagged(1, r.format), tagged(2, String(r.version)), tagged(3, r.repository_identity), tagged(4, r.object_format), tagged(5, r.base_oid), tagged(6, r.tip_oid), tagged(7, r.base_tree_oid), tagged(8, r.result_tree_oid), tagged(9, r.changeset_algorithm), tagged(10, r.changeset_digest), new Uint8Array([11]), u64(r.commits.length), ...r.commits.map((m) => tagged(12, m.commit_oid))));
}
const exactKeys = (value: unknown, keys: string[]) => !!value && typeof value === "object" && !Array.isArray(value) && Object.keys(value as object).every((key) => keys.includes(key) || keys.includes(`${key}?`)) && keys.filter((key) => !key.endsWith("?")).every((key) => key in (value as object));
const oidOrNull = (x: unknown) => x == null || typeof x === "string" && HEX40.test(x);

export async function validRange(value: unknown, pathId: string): Promise<boolean> {
  if (!exactKeys(value, ["range_id", "format", "version", "repository_identity", "object_format", "base_oid", "tip_oid", "base_tree_oid", "result_tree_oid", "commits", "changeset_algorithm", "changeset_digest"])) return false;
  const r = value as RangeEvidence;
  if (r.range_id !== pathId || !HEX64.test(pathId) || r.format !== "cvc.range-evidence/v1" || r.version !== 1 || r.object_format !== "sha1" || !HEX64.test(r.repository_identity) || ![r.base_oid, r.tip_oid, r.base_tree_oid, r.result_tree_oid].every((x) => HEX40.test(x)) || r.changeset_algorithm !== "cvc.changeset/v1" || !HEX64.test(r.changeset_digest) || !Array.isArray(r.commits) || !r.commits.length || r.commits.length > MAX_RANGE_COMMITS) return false;
  const seen = new Set<string>();
  if (!r.commits.every((m) => exactKeys(m, ["commit_oid"]) && HEX40.test(m.commit_oid) && !seen.has(m.commit_oid) && !!seen.add(m.commit_oid))) return false;
  return r.range_id === await canonicalRangeId(r);
}
export async function validEvent(value: unknown, pathId: string): Promise<boolean> {
  if (!exactKeys(value, ["event_id", "interaction_id", "target_commit", "relation", "evidence", "origin", "source_event_ids", "old_oid?", "new_oid?", "range_id?", "linked_by?"])) return false;
  const e = value as DerivationEvent;
  if (e.event_id !== pathId || !HEX64.test(pathId) || !UUID.test(e.interaction_id) || !HEX40.test(e.target_commit) || !["generated", "temporal", "verified", "rewrite_exact", "squash_exact"].includes(e.relation) || !exactKeys(e.evidence, ["version", "kind"]) || e.evidence.version !== 1 || !["locally_observed", "imported_legacy", "remote_assertion"].includes(e.evidence.kind) || !["local_hook", "local_linker", "remote_assertion", "legacy_import"].includes(e.origin) || !Array.isArray(e.source_event_ids) || e.source_event_ids.some((id) => typeof id !== "string" || !id || id.length > 512) || e.source_event_ids.some((id, i) => i && e.source_event_ids[i - 1] >= id) || !oidOrNull(e.old_oid) || !oidOrNull(e.new_oid) || (e.range_id != null && (typeof e.range_id !== "string" || !HEX64.test(e.range_id))) || (e.linked_by != null && (typeof e.linked_by !== "string" || !e.linked_by || e.linked_by.length > 1024 * 1024))) return false;
  if (e.relation === "rewrite_exact" && !(e.source_event_ids.length && e.old_oid && e.new_oid === e.target_commit && !e.range_id && e.evidence.kind === "locally_observed" && e.origin === "local_hook")) return false;
  if (e.relation === "squash_exact" && !(e.source_event_ids.length && !e.old_oid && !e.new_oid && e.range_id && e.evidence.kind === "locally_observed" && e.origin === "local_hook")) return false;
  if (["generated", "temporal", "verified"].includes(e.relation) && (e.old_oid || e.new_oid || e.range_id)) return false;
  return e.event_id === await canonicalEventId(e);
}

type ByteBudget = { bytes: number; pending?: Promise<void> };

/** Serializes readers sharing a budget, preventing parallel streams overspending it. */
export async function readBoundedJson(url: string, token: string, budget: ByteBudget, limit = MAX_BLOB): Promise<unknown> {
  const previous = budget.pending ?? Promise.resolve();
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  budget.pending = previous.then(() => gate);
  await previous;
  try { return await readBoundedJsonUnlocked(url, token, budget, limit); }
  finally { release(); }
}

async function readBoundedJsonUnlocked(url: string, token: string, budget: ByteBudget, limit: number): Promise<unknown> {
  const parsed = new URL(url);
  if (parsed.origin !== "https://api.github.com") throw new Error("Refusing to send reviewer credentials off GitHub API");
  const response = await fetch(url, { redirect: "error", headers: { Authorization: `token ${token}`, Accept: "application/vnd.github.v3.raw" } });
  if (!response.ok) throw new Error("Failed to fetch FORMAT5 artifact");
  const declared = response.headers.get("content-length");
  if (declared && (!/^\d+$/.test(declared) || Number(declared) > limit || budget.bytes + Number(declared) > MAX_TOTAL)) throw new Error("FORMAT5 artifact exceeds safe size limits");
  if (!response.body) throw new Error("FORMAT5 artifact has no readable body");
  const reader = response.body.getReader(); const chunks: Uint8Array[] = []; let size = 0;
  while (true) { const { done, value } = await reader.read(); if (done) break; size += value.byteLength; if (size > limit || budget.bytes + size > MAX_TOTAL) { await reader.cancel(); throw new Error("FORMAT5 artifact exceeds safe size limits"); } chunks.push(value); }
  budget.bytes += size;
  let text: string;
  try { text = new TextDecoder("utf-8", { fatal: true }).decode(bytes(...chunks)); }
  catch { throw new Error("Invalid FORMAT5 UTF-8"); }
  try { return JSON.parse(text); } catch { throw new Error("Invalid FORMAT5 JSON"); }
}

async function namespace(client: GithubClient, owner: string, repo: string, rootSha: string, name: "events" | "ranges", token: string, max: number, budget: { bytes: number }, validate: (value: unknown, id: string) => Promise<boolean>) {
  if (!HEX40.test(rootSha)) throw new Error(`Invalid FORMAT5 ${name} root SHA`);
  const { data: root } = await client.octokit.rest.git.getTree({ owner, repo, tree_sha: rootSha, recursive: "false" });
  if (root.truncated) throw new Error(`FORMAT5 ${name} tree is truncated`);
  const result: unknown[] = [];
  for (const shard of root.tree) {
    if (!shard.path || shard.type !== "tree" || !/^[0-9a-f]{2}$/.test(shard.path) || !shard.sha || !HEX40.test(shard.sha)) throw new Error(`Invalid FORMAT5 ${name} shard`);
    const { data: entries } = await client.octokit.rest.git.getTree({ owner, repo, tree_sha: shard.sha, recursive: "false" });
    if (entries.truncated) throw new Error(`FORMAT5 ${name} shard is truncated`);
    for (const entry of entries.tree as TreeEntry[]) {
      const id = entry.path?.endsWith(".json") ? entry.path.slice(0, -5) : "";
      if (entry.type !== "blob" || !HEX64.test(id) || id.slice(0, 2) !== shard.path || !entry.sha || !HEX40.test(entry.sha) || (entry.size !== undefined && (!Number.isSafeInteger(entry.size) || entry.size < 0 || entry.size > MAX_BLOB)) || ++result.length > max) throw new Error(`Invalid or excessive FORMAT5 ${name} entry`);
      const payload = await readBoundedJson(`https://api.github.com/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/git/blobs/${entry.sha}`, token, budget);
      if (!await validate(payload, id)) throw new Error(`Invalid FORMAT5 ${name} payload`);
      result.push(payload);
    }
  }
  return result;
}

/** Fetches independent v5 evidence namespaces. The caller must fail closed on any error. */
export async function fetchFormat5Evidence(client: GithubClient, owner: string, repo: string, eventsSha: string | undefined, rangesSha: string | undefined, token: string) {
  if (!eventsSha || !rangesSha) throw new Error("FORMAT5 requires events and ranges trees");
  const budget = { bytes: 0 };
  const events = await namespace(client, owner, repo, eventsSha, "events", token, MAX_EVENTS, budget, validEvent) as DerivationEvent[];
  const ranges = await namespace(client, owner, repo, rangesSha, "ranges", token, MAX_RANGES, budget, validRange) as RangeEvidence[];
  const rangeIds = new Set(ranges.map((r) => r.range_id));
  if (events.some((e) => e.relation === "squash_exact" && !rangeIds.has(e.range_id!))) throw new Error("FORMAT5 squash event has dangling range");
  const byId = new Map(events.map((event) => [event.event_id, event]));
  const legacy = /^legacy:([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}):([0-9a-f]{40})$/;
  for (const event of events) for (const source of event.source_event_ids) {
    const sourceEvent = byId.get(source);
    if (sourceEvent) { if (sourceEvent.interaction_id !== event.interaction_id) throw new Error("FORMAT5 source crosses interactions"); continue; }
    const sourceMatch = legacy.exec(source);
    if (!sourceMatch || sourceMatch[1] !== event.interaction_id) throw new Error("FORMAT5 source is malformed or unresolved");
  }
  const visiting = new Set<string>(); const visited = new Set<string>();
  const visit = (id: string) => { if (visiting.has(id)) throw new Error("FORMAT5 derivation graph is cyclic"); if (visited.has(id)) return; visiting.add(id); for (const source of byId.get(id)!.source_event_ids) if (byId.has(source)) visit(source); visiting.delete(id); visited.add(id); };
  for (const event of events) visit(event.event_id);
  return { events, ranges };
}

export function eventLink(e: DerivationEvent): ArtifactLinkData {
  return { interaction_id: e.interaction_id, git_commit_hash: e.target_commit, link_type: e.relation === "rewrite_exact" || e.relation === "squash_exact" ? "generated" : e.relation, derivation_event: e };
}
