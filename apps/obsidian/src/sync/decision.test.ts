import { describe, expect, test } from "vitest";
import { decide, SyncDecision } from "./decision";

// Ported alongside decision.ts from larknotes `crates/sync/src/decision.rs`.
describe("decide", () => {
  test("no change", () => {
    expect(decide(false, false, true)).toBe(SyncDecision.NoChange);
  });

  test("only local changed → push", () => {
    expect(decide(true, false, true)).toBe(SyncDecision.PushLocal);
  });

  test("only remote changed → pull", () => {
    expect(decide(false, true, true)).toBe(SyncDecision.PullRemote);
  });

  test("both changed → conflict", () => {
    expect(decide(true, true, true)).toBe(SyncDecision.BothModified);
  });

  test("no baseline → new file regardless of change flags", () => {
    expect(decide(true, false, false)).toBe(SyncDecision.NewFile);
    expect(decide(false, false, false)).toBe(SyncDecision.NewFile);
    expect(decide(true, true, false)).toBe(SyncDecision.NewFile);
  });
});
