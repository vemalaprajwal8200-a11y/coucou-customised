import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { sanitizeAssistantText } from "../src/core/text";

describe("assistant response normalization", () => {
  it("removes invisible and control characters", () => {
    assert.equal(sanitizeAssistantText("Hello\u200B\u0000world"), "Helloworld");
  });

  it("collapse pathological repeated characters", () => {
    assert.equal(sanitizeAssistantText("A".repeat(50)), "AA");
  });

  it("bounds output before the UI or TTS receives it", () => {
    const longText = "ab".repeat(2500);
    assert.equal(sanitizeAssistantText(longText).length, 2048);
  });
});
