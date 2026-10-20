import { describe, expect, it } from "vitest";
import { validTombstone } from "./cvc";

// A real id written by the Claude Code harness: RFC 9562 version 8 with the RFC variant bits.
// (Every other producer uses version 4.)
const ID_V8 = "3ec4e81b-3ffc-8d60-b160-5c0b3a2407db";
const ID_V4 = "123e4567-e89b-42d3-a456-426614174000";

/** An id of the canonical shape with its version and variant nibbles chosen (each is one hex digit). */
const idWith = (version: string, variant: string) => `123e4567-e89b-${version}2d3-${variant}456-426614174000`;

/** The tombstone payload `cvc redact` publishes for `id`, with fields overridden. */
function payload(id: string, overrides: Record<string, unknown> = {}) {
  return {
    format: "cvc.tombstone/v1",
    version: 1,
    interaction_id: id,
    deleted_at: "2026-01-01T00:00:00Z",
    reason_code: "security",
    ...overrides,
  };
}

describe("validTombstone: which interaction ids it accepts", () => {
  it("accepts a payload for a version 8 id, which is what the Claude Code harness writes", () => {
    expect(validTombstone(payload(ID_V8), ID_V8)).toBe(true);
  });

  it("still accepts a payload for a version 4 id", () => {
    expect(validTombstone(payload(ID_V4), ID_V4)).toBe(true);
  });

  it.each(["1", "2", "3", "4", "5", "6", "7", "8"])("accepts RFC 9562 version %s", (version) => {
    const id = idWith(version, "a");
    expect(validTombstone(payload(id), id)).toBe(true);
  });

  it.each(["8", "9", "a", "b"])("accepts the RFC variant digit %s", (variant) => {
    const id = idWith("8", variant);
    expect(validTombstone(payload(id), id)).toBe(true);
  });

  it.each(["0", "9", "a", "b", "c", "d", "e", "f"])("refuses version digit %s, which RFC 9562 does not define", (version) => {
    for (const variant of ["8", "9", "a", "b"]) {
      const id = idWith(version, variant);
      expect(validTombstone(payload(id), id), id).toBe(false);
    }
  });

  it.each(["0", "1", "7", "c", "d", "e", "f"])("refuses variant digit %s, which is not the RFC variant", (variant) => {
    for (const version of ["4", "8"]) {
      const id = idWith(version, variant);
      expect(validTombstone(payload(id), id), id).toBe(false);
    }
  });

  it("refuses the all-zero and all-one ids", () => {
    for (const id of ["00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff"]) {
      expect(validTombstone(payload(id), id), id).toBe(false);
    }
  });

  it("refuses an id that is not in the hyphenated form", () => {
    for (const id of [ID_V8.replaceAll("-", ""), `{${ID_V8}}`, `urn:uuid:${ID_V8}`, ` ${ID_V8}`, `${ID_V8}\n`, ID_V8.slice(0, -1), `${ID_V8}0`, `0${ID_V8}`, "", "not-an-id"]) {
      expect(validTombstone(payload(id), id), JSON.stringify(id)).toBe(false);
    }
  });

  // The pattern ignores case, and that is unchanged: the lowercase-only form is enforced where the id
  // comes from, on the path (validateTombstoneTree), before a payload is ever read.
  it.each([
    ["version 4", ID_V4],
    ["version 8", ID_V8],
  ])("keeps its case rule for a %s id: the pattern ignores case, and the payload must repeat the path's id exactly", (_label, id) => {
    const upper = id.toUpperCase();
    expect(validTombstone(payload(upper), upper)).toBe(true);
    expect(validTombstone(payload(id), upper)).toBe(false);
    expect(validTombstone(payload(upper), id)).toBe(false);
  });
});

describe("validTombstone: every other check, for a version 8 id", () => {
  const valid = (overrides: Record<string, unknown>) => validTombstone(payload(ID_V8, overrides), ID_V8);

  it("requires interaction_id to be the id on the path", () => {
    expect(validTombstone(payload(ID_V4), ID_V8)).toBe(false);
    expect(validTombstone(payload(ID_V8), ID_V4)).toBe(false);
    expect(valid({ interaction_id: 5 })).toBe(false);
    expect(valid({ interaction_id: null })).toBe(false);
    expect(validTombstone({ ...payload(ID_V8), interaction_id: undefined }, ID_V8)).toBe(false);
  });

  it.each([
    ["another format", { format: "cvc.tombstone/v2" }],
    ["no format", { format: undefined }],
    ["another version", { version: 2 }],
    ["a version that is a string", { version: "1" }],
    ["a deleted_at that is not a date", { deleted_at: "last tuesday" }],
    ["a deleted_at that is not text", { deleted_at: 1735689600 }],
    ["a reason code outside the vocabulary", { reason_code: "because" }],
    ["no reason code", { reason_code: undefined }],
    ["a free-text field the format does not have", { prompt: "the redacted text" }],
    ["a previous_node_oid that is too short", { previous_node_oid: "abc123" }],
    ["a previous_node_oid that is not hex", { previous_node_oid: "g".repeat(40) }],
    ["a previous_node_oid that is not text", { previous_node_oid: 5 }],
    ["a previous_node_oid of an in-between length", { previous_node_oid: "a".repeat(50) }],
  ])("refuses %s", (_label, overrides) => {
    expect(valid(overrides)).toBe(false);
  });

  it.each([
    ["no previous_node_oid", {}],
    ["a null previous_node_oid", { previous_node_oid: null }],
    ["a SHA-1 previous_node_oid", { previous_node_oid: "a".repeat(40) }],
    ["a SHA-256 previous_node_oid", { previous_node_oid: "0123456789abcdef".repeat(4) }],
    ["a retention reason", { reason_code: "retention" }],
    ["a user_requested reason", { reason_code: "user_requested" }],
  ])("accepts %s", (_label, overrides) => {
    expect(valid(overrides)).toBe(true);
  });

  it.each([null, undefined, "a string", 42, true])("refuses %j, which is not an object", (value) => {
    expect(validTombstone(value, ID_V8)).toBe(false);
  });

  it("refuses an array", () => {
    expect(validTombstone([], ID_V8)).toBe(false);
    expect(validTombstone([payload(ID_V8)], ID_V8)).toBe(false);
  });
});
