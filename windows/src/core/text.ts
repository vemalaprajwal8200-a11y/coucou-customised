const MAX_ASSISTANT_TEXT_LENGTH = 2048;

export function sanitizeAssistantText(value: string): string {
  const normalized = value
    .normalize("NFKC")
    .replace(/[\u0000-\u001F\u007F-\u009F\u200B-\u200D\u2060\uFEFF]/gu, "")
    .replace(/\s+/gu, " ")
    .trim();

  if (!normalized) return "";

  const characters = Array.from(normalized);
  const collapsed: string[] = [];
  let lastCharacter = "";
  let repeatCount = 0;

  for (const character of characters) {
    if (character === lastCharacter) {
      repeatCount += 1;
      if (repeatCount > 2) continue;
    } else {
      lastCharacter = character;
      repeatCount = 1;
    }
    collapsed.push(character);
  }

  return collapsed.join("").slice(0, MAX_ASSISTANT_TEXT_LENGTH);
}
