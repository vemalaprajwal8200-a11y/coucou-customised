import { Bridge, IS_TAURI } from "./bridge";
import { State } from "./state";

const ENGINE_ORDER = ["webSpeech", "sapi"] as const;
const VOICE_PREFERENCE = [
  "Microsoft Aria Natural",
  "Microsoft Jenny",
  "Microsoft Zira",
  "Microsoft David",
];
const MAX_UTTERANCE_LENGTH = 190;
const VOICE_TIMEOUT_MS = 2000;
const POLL_INTERVAL_MS = 100;

type TtsEngine = (typeof ENGINE_ORDER)[number];
type StateListener = (speaking: boolean, error?: string) => void;
type QueueItem = { text: string };

let queue: QueueItem[] = [];
let streamBuffer = "";
let streamOpen = false;
let running = false;
let generation = 0;
let activeUtterance: SpeechSynthesisUtterance | null = null;
const listeners = new Set<StateListener>();

function notify(speaking: boolean, error?: string) {
  for (const listener of listeners) listener(speaking, error);
}

export function isSpeaking(): boolean {
  return running;
}

export function onStateChange(callback: StateListener): () => void {
  listeners.add(callback);
  callback(running);
  return () => listeners.delete(callback);
}

export async function availableVoices(): Promise<SpeechSynthesisVoice[]> {
  if (!("speechSynthesis" in window)) return [];
  const synthesis = window.speechSynthesis;
  const localVoices = () => synthesis.getVoices().filter(
    (voice) => voice.localService && isEnglishVoice(voice),
  );
  const initial = localVoices();
  if (initial.length) return initial;

  return new Promise((resolve) => {
    const finish = () => {
      window.clearTimeout(timeout);
      synthesis.removeEventListener("voiceschanged", changed);
      resolve(localVoices());
    };
    const changed = () => {
      if (localVoices().length) finish();
    };
    const timeout = window.setTimeout(finish, VOICE_TIMEOUT_MS);
    synthesis.addEventListener("voiceschanged", changed);
  });
}

function isEnglishVoice(voice: SpeechSynthesisVoice): boolean {
  return /^en(?:-|$)/i.test(voice.lang.trim());
}

void availableVoices().catch((error: unknown) => {
  console.error("[coucou] could not initialize local speech voices", error);
});

function preferredVoice(voices: SpeechSynthesisVoice[]): SpeechSynthesisVoice | undefined {
  const selected = State.settings.ttsVoice;
  if (selected) {
    const match = voices.find((voice) => voice.voiceURI === selected && isEnglishVoice(voice));
    if (match) return match;
  }
  for (const preferred of VOICE_PREFERENCE) {
    const match = voices.find((voice) =>
      isEnglishVoice(voice) && voice.name.toLowerCase().includes(preferred.toLowerCase()));
    if (match) return match;
  }
  return voices.find((voice) => /^en-us\b/i.test(voice.lang))
    ?? voices.find((voice) => /^en-in\b/i.test(voice.lang))
    ?? voices.find(isEnglishVoice);
}

export function cleanSpeechText(text: string): string {
  return text
    .replace(/```[\s\S]*?(?:```|$)/g, " ")
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/https?:\/\/\S+/gi, " ")
    .replace(/<[^>]*>/g, " ")
    .replace(/(?:\*\*|__|~~|`|[*_#>])/g, "")
    .replace(/[\p{Extended_Pictographic}\p{Emoji_Presentation}\p{Emoji_Modifier}\p{Regional_Indicator}\uFE0F\u200D\u20E3]/gu, "")
    .replace(/[ \t]+/g, " ")
    .replace(/ *\n+ */g, "\n")
    .trim();
}

function splitLongSentence(sentence: string): string[] {
  const chunks: string[] = [];
  let current = "";
  for (const word of sentence.split(/\s+/)) {
    if (!word) continue;
    if (word.length > MAX_UTTERANCE_LENGTH) {
      if (current) chunks.push(current);
      current = "";
      for (let offset = 0; offset < word.length; offset += MAX_UTTERANCE_LENGTH) {
        chunks.push(word.slice(offset, offset + MAX_UTTERANCE_LENGTH));
      }
    } else if (!current) {
      current = word;
    } else if (`${current} ${word}`.length <= MAX_UTTERANCE_LENGTH) {
      current += ` ${word}`;
    } else {
      chunks.push(current);
      current = word;
    }
  }
  if (current) chunks.push(current);
  return chunks;
}

function splitSpeechText(text: string): string[] {
  const cleaned = cleanSpeechText(text);
  const sentences = cleaned.match(/[^.!?\n]+[.!?]*/g) ?? [];
  const chunks: string[] = [];
  let shortSentence = "";
  for (const raw of sentences) {
    const sentence = raw.trim();
    if (!sentence) continue;
    shortSentence = shortSentence ? `${shortSentence} ${sentence}` : sentence;
    if (shortSentence.length >= 20) {
      chunks.push(...splitLongSentence(shortSentence));
      shortSentence = "";
    }
  }
  if (shortSentence) chunks.push(...splitLongSentence(shortSentence));
  return chunks;
}

function enqueue(chunks: string[]) {
  queue.push(...chunks.map((text) => ({ text })));
  for (const wake of streamWaiters.splice(0)) wake();
  if (!running && queue.length) void drain(generation);
}

const streamWaiters: Array<() => void> = [];

function sentenceBoundary(text: string): number {
  let boundary = -1;
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] === "\n") boundary = index + 1;
    else if (".!?".includes(text[index]) && (index + 1 === text.length || /\s/.test(text[index + 1]))) {
      boundary = index + 1;
    }
  }
  return boundary;
}

export function speakStreamChunk(textDelta: string): void {
  streamOpen = true;
  streamBuffer += textDelta;
  while (streamBuffer) {
    const openingFence = streamBuffer.indexOf("```");
    if (openingFence >= 0) {
      const beforeCode = streamBuffer.slice(0, openingFence);
      if (beforeCode.trim()) enqueue(splitSpeechText(beforeCode));
      const closingFence = streamBuffer.indexOf("```", openingFence + 3);
      if (closingFence < 0) {
        streamBuffer = streamBuffer.slice(openingFence);
        return;
      }
      streamBuffer = streamBuffer.slice(closingFence + 3);
      continue;
    }
    const boundary = sentenceBoundary(streamBuffer);
    if (boundary < 0) return;
    const ready = streamBuffer.slice(0, boundary);
    streamBuffer = streamBuffer.slice(boundary);
    enqueue(splitSpeechText(ready));
  }
}

export function finishStream(): void {
  if (streamBuffer) enqueue(splitSpeechText(streamBuffer));
  streamBuffer = "";
  streamOpen = false;
  for (const wake of streamWaiters.splice(0)) wake();
}

export async function speak(text: string): Promise<void> {
  try {
    await stop();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error("[coucou] could not stop previous speech before starting a reply", error);
    notify(running, message);
    return;
  }
  streamBuffer = "";
  streamOpen = false;
  const chunks = splitSpeechText(text);
  if (!chunks.length) return;
  enqueue(chunks);
  await new Promise<void>((resolve) => {
    const unsubscribe = onStateChange((speaking) => {
      if (!speaking) {
        unsubscribe();
        resolve();
      }
    });
  });
}

export async function stop(): Promise<void> {
  generation += 1;
  queue = [];
  streamBuffer = "";
  streamOpen = false;
  activeUtterance = null;
  if ("speechSynthesis" in window) window.speechSynthesis.cancel();
  for (const wake of streamWaiters.splice(0)) wake();
  if (IS_TAURI) await Bridge.ttsStop();
  if (running) {
    running = false;
    notify(false);
  }
}

async function speakWithWebSpeech(text: string, utteranceGeneration: number): Promise<void> {
  const voice = preferredVoice(await availableVoices());
  if (!voice) throw new Error("No local Web Speech voice is available.");
  if (utteranceGeneration !== generation) return;

  await new Promise<void>((resolve, reject) => {
    const utterance = new SpeechSynthesisUtterance(text);
    activeUtterance = utterance;
    utterance.voice = voice;
    utterance.lang = voice.lang;
    utterance.rate = State.settings.ttsRate;
    utterance.volume = State.settings.ttsVolume;
    utterance.onend = () => {
      if (activeUtterance === utterance) activeUtterance = null;
      resolve();
    };
    utterance.onerror = (event) => {
      if (activeUtterance === utterance) activeUtterance = null;
      if (utteranceGeneration !== generation || event.error === "canceled") resolve();
      else reject(new Error(`Web Speech failed: ${event.error}`));
    };
    window.speechSynthesis.speak(utterance);
  });
}

async function speakWithSapi(text: string, utteranceGeneration: number): Promise<void> {
  if (!IS_TAURI) throw new Error("Windows SAPI is available only in the Coucou app.");
  await Bridge.ttsSpeak(text, State.settings.ttsRate, State.settings.ttsVolume);
  while (utteranceGeneration === generation) {
    if (!await Bridge.ttsIsSpeaking()) return;
    await new Promise((resolve) => window.setTimeout(resolve, POLL_INTERVAL_MS));
  }
  await Bridge.ttsStop();
}

async function playChunk(text: string, utteranceGeneration: number): Promise<void> {
  const forcedEngine = State.settings.ttsEngine;
  const engineOrder: readonly TtsEngine[] = forcedEngine === "auto"
    ? ENGINE_ORDER
    : [forcedEngine];
  let lastError: unknown;
  for (const engine of engineOrder) {
    if (utteranceGeneration !== generation) return;
    try {
      if (engine === "webSpeech") await speakWithWebSpeech(text, utteranceGeneration);
      else await speakWithSapi(text, utteranceGeneration);
      return;
    } catch (error) {
      lastError = error;
      if (forcedEngine !== "auto") throw error;
      console.warn(`[coucou] ${engine} voice failed; trying the next local engine`, error);
    }
  }
  throw lastError instanceof Error ? lastError : new Error("No offline speech engine is available.");
}

async function drain(drainGeneration: number): Promise<void> {
  if (running) return;
  running = true;
  notify(true);
  let failure: string | undefined;
  try {
    while (drainGeneration === generation) {
      const item = queue.shift();
      if (item) {
        await playChunk(item.text, drainGeneration);
        continue;
      }
      if (!streamOpen) break;
      await new Promise<void>((resolve) => streamWaiters.push(resolve));
    }
  } catch (error) {
    queue = [];
    streamBuffer = "";
    streamOpen = false;
    failure = error instanceof Error ? error.message : String(error);
    console.error("[coucou] offline speech failed", error);
  } finally {
    if (drainGeneration === generation) {
      running = false;
      notify(false, failure);
    }
  }
}
