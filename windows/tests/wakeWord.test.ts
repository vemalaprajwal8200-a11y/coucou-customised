import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { extractWakeCommand, isWakeLeadOnly } from "../src/core/wakeWord";

describe("wake-word response timing", () => {
  it("treats the exact wake phrase as an immediate signal", () => {
    assert.equal(isWakeLeadOnly("Hey Macha"), false);
    assert.equal(extractWakeCommand("Hey Macha"), "");
  });

  it("extracts a command after the wake phrase", () => {
    assert.equal(extractWakeCommand("Hey Macha, tell me the weather"), "tell me the weather");
  });
});
