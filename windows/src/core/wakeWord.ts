const STT_ENDPOINT = "http://127.0.0.1:5005/transcribe";
const STT_HEALTH_ENDPOINT = "http://127.0.0.1:5005/health";
const STT_TIMEOUT_MS = 15_000;
const STT_STARTUP_TIMEOUT_MS = 5_000;
const STT_STARTUP_RETRY_MS = 250;
const DEFAULT_SPEECH_THRESHOLD = 0.006;
const PRE_ROLL_MS = 280;
const SILENCE_MS = 220;
const WAKE_SILENCE_MS = 600;
const MIN_SPEECH_MS = 120;
const MAX_SPEECH_MS = 12_000;
const CALIBRATION_MS = 5000;
const FILLER_WORDS = new Set(["um", "uh", "oh", "ah", "er", "okay", "ok", "so", "well"]);

async function waitForSttReady(): Promise<void> {
  const deadline = performance.now() + STT_STARTUP_TIMEOUT_MS;
  while (performance.now() < deadline) {
    try {
      const response = await fetch(STT_HEALTH_ENDPOINT, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) return;
    } catch {
      // The backend may still be loading the Whisper model. Retry briefly.
    }
    await new Promise((resolve) => window.setTimeout(resolve, STT_STARTUP_RETRY_MS));
  }
  throw new Error("Local speech service is not ready. Start Coucou with the voice service running.");
}

const WAKE_WORD_VARIANTS: Record<string, readonly string[]> = {
  hey: ["hey", "hay", "hai", "hi", "he", "ay"],
  hay: ["hey", "hay", "hai", "hi", "he", "ay"],
  hai: ["hey", "hay", "hai", "hi", "he", "ay"],
  hi: ["hey", "hay", "hai", "hi", "he", "ay"],
  he: ["hey", "hay", "hai", "hi", "he", "ay"],
  ay: ["hey", "hay", "hai", "hi", "he", "ay"],
  macha: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
  matcha: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
  masha: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
  machaa: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
  macho: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
  mocha: ["macha", "matcha", "masha", "machaa", "macho", "mocha", "mark"],
};

function wakeWords(value: string): string[] {
  return value.toLocaleLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

function configuredWakeWords(pronunciation: string): string[] {
  const configured = wakeWords(pronunciation);
  return configured.length >= 2 ? configured.slice(0, 2) : ["hey", "macha"];
}

function wakeWordMatches(actual: string, configured: string): boolean {
  return (WAKE_WORD_VARIANTS[configured] ?? [configured]).includes(actual);
}

function tokenStart(tokens: RegExpMatchArray[]): number {
  let start = 0;
  while (start < tokens.length && FILLER_WORDS.has(tokens[start][0].toLocaleLowerCase())) {
    start += 1;
  }
  return start;
}

export function extractWakeCommand(
  transcript: string,
  pronunciation = "Hey Macha",
): string | null {
  const actual = [...transcript.matchAll(/[\p{L}\p{N}]+/gu)];
  const expected = configuredWakeWords(pronunciation);
  const start = tokenStart(actual);
  if (actual.length - start < expected.length) return null;
  for (const [index, word] of expected.entries()) {
    if (!wakeWordMatches(actual[start + index][0].toLocaleLowerCase(), word)) return null;
  }
  const last = actual[start + expected.length - 1];
  const phraseEnd = last.index! + last[0].length;
  return transcript.slice(phraseEnd).replace(/^[\s,:;.!?-]+/, "").trim();
}

export function isWakeLeadOnly(transcript: string, pronunciation = "Hey Macha"): boolean {
  const actual = [...transcript.matchAll(/[\p{L}\p{N}]+/gu)];
  const expected = configuredWakeWords(pronunciation);
  const start = tokenStart(actual);
  return actual.length - start === 1 && wakeWordMatches(actual[start][0].toLocaleLowerCase(), expected[0]);
}

export interface WakeWordListener {
  start(
    onUtterance: (text: string) => void,
    onError: (message: string) => void,
    pronunciation: string,
    threshold: number,
    useWakeHint?: boolean,
  ): Promise<void>;
  stop(): void;
  isActive(): boolean;
}

export interface WakeWordCalibration {
  transcript: string;
  threshold: number;
}

function encodeWav(frames: Float32Array[], sampleRate: number): Blob {
  const samples = frames.reduce((total, frame) => total + frame.length, 0);
  const buffer = new ArrayBuffer(44 + samples * 2);
  const view = new DataView(buffer);
  const writeText = (offset: number, value: string) => {
    for (let index = 0; index < value.length; index += 1) {
      view.setUint8(offset + index, value.charCodeAt(index));
    }
  };
  writeText(0, "RIFF");
  view.setUint32(4, 36 + samples * 2, true);
  writeText(8, "WAVE");
  writeText(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeText(36, "data");
  view.setUint32(40, samples * 2, true);

  let offset = 44;
  for (const frame of frames) {
    for (const sample of frame) {
      const value = Math.max(-1, Math.min(1, sample));
      view.setInt16(offset, value < 0 ? value * 0x8000 : value * 0x7fff, true);
      offset += 2;
    }
  }
  return new Blob([buffer], { type: "audio/wav" });
}

function streamRms(samples: Float32Array): number {
  let sumSquares = 0;
  for (const sample of samples) sumSquares += sample * sample;
  return Math.sqrt(sumSquares / samples.length);
}

async function transcribeWakeClip(audio: Blob, pronunciation: string): Promise<string> {
  const form = new FormData();
  form.append("audio", audio, "hey-macha.wav");
  form.append("wake_hint", pronunciation);
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), STT_TIMEOUT_MS);
  try {
    const response = await fetch(STT_ENDPOINT, {
      method: "POST",
      body: form,
      signal: controller.signal,
    });
    if (!response.ok) {
      const body: unknown = await response.json().catch(() => null);
      const message = typeof body === "object" && body !== null && "error" in body
        && typeof body.error === "string"
        ? body.error
        : `Wake-word transcription failed (HTTP ${response.status}).`;
      throw new Error(message);
    }
    const result: unknown = await response.json();
    if (typeof result === "object" && result !== null && "text" in result
      && typeof result.text === "string" && result.text.trim()) {
      return result.text.trim();
    }
    throw new Error("No speech detected. Say “Hey Macha” clearly and try again.");
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new Error("Local Whisper timed out. Try recording again.");
    }
    throw error;
  } finally {
    window.clearTimeout(timeout);
  }
}

export async function recordWakePronunciation(): Promise<WakeWordCalibration> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("Microphone recording is not available in this window.");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
    },
  });
  let context: AudioContext | null = null;
  let source: MediaStreamAudioSourceNode | null = null;
  let analyser: ScriptProcessorNode | null = null;
  let mutedOutput: GainNode | null = null;
  let recorder: MediaRecorder | null = null;
  const voiceLevels: number[] = [];
  let collectLevels = true;
  try {
    context = new AudioContext();
    await context.resume();
    source = context.createMediaStreamSource(stream);
    analyser = context.createScriptProcessor(2048, 1, 1);
    mutedOutput = context.createGain();
    mutedOutput.gain.value = 0;
    analyser.onaudioprocess = (event) => {
      if (!collectLevels) return;
      const samples = event.inputBuffer.getChannelData(0);
      const copy = new Float32Array(samples);
      const level = streamRms(copy);
      if (level > 0.002) voiceLevels.push(level);
    };
    source.connect(analyser);
    analyser.connect(mutedOutput);
    mutedOutput.connect(context.destination);

    const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
      ? "audio/webm;codecs=opus"
      : "audio/webm";
    recorder = new MediaRecorder(stream, { mimeType });
    const audioChunks: Blob[] = [];
    recorder.addEventListener("dataavailable", (event) => {
      if (event.data.size > 0) audioChunks.push(event.data);
    });
    const stopped = new Promise<void>((resolve, reject) => {
      recorder!.addEventListener("stop", () => resolve(), { once: true });
      recorder!.addEventListener("error", () => reject(new Error("Wake phrase recording failed.")), {
        once: true,
      });
    });
    recorder.start(250);
    await new Promise((resolve) => window.setTimeout(resolve, CALIBRATION_MS));
    collectLevels = false;
    recorder.stop();
    await stopped;
    if (!voiceLevels.length) {
      throw new Error("No voice level detected. Check your microphone and try again.");
    }

    const calibrationAudio = new Blob(audioChunks, { type: mimeType });
    const transcript = await transcribeWakeClip(calibrationAudio, "Hey Macha");
    const phrase = transcript.match(
      /\b(hey|hay|hai|hi|he|ay)\s+(macha|matcha|masha|ma\s+cha|machaa|macho|mocha)\b/i,
    );
    if (!phrase) {
      throw new Error(`Whisper heard “${transcript}”. Please try again and say “Hey Macha”.`);
    }
    const savedPronunciation = `${phrase[1]} ${phrase[2].replace(/\s+/g, " ")}`;
    voiceLevels.sort((a, b) => a - b);
    const typicalSpeechLevel = voiceLevels[Math.floor(voiceLevels.length * 0.2)];
    const threshold = Math.round(Math.max(0.003, Math.min(0.03, typicalSpeechLevel * 0.35)) * 10000) / 10000;
    return { transcript: savedPronunciation, threshold };
  } finally {
    collectLevels = false;
    if (recorder?.state === "recording") recorder.stop();
    if (analyser) analyser.onaudioprocess = null;
    source?.disconnect();
    analyser?.disconnect();
    mutedOutput?.disconnect();
    if (context) {
      await context.close().catch((error: unknown) => {
        console.error("[coucou] could not close pronunciation recording context", error);
      });
    }
    stream.getTracks().forEach((track) => track.stop());
  }
}

export function createWakeWordListener(): WakeWordListener {
  let stream: MediaStream | null = null;
  let context: AudioContext | null = null;
  let processor: ScriptProcessorNode | null = null;
  let source: MediaStreamAudioSourceNode | null = null;
  let mutedOutput: GainNode | null = null;
  let speechFrames: Float32Array[] = [];
  let preRollFrames: Float32Array[] = [];
  let preRollSamples = 0;
  let speechStartedAt = 0;
  let speechSilenceMs = 0;
  let wakeWindowSent = false;
  let active = false;
  let requestRunning = false;
  let generation = 0;
  let activeRequest: AbortController | null = null;
  let threshold = DEFAULT_SPEECH_THRESHOLD;
  let pronunciation = "Hey Macha";
  let useWakeHint = true;
  const pendingAudio: Array<{ audio: Blob; run: number }> = [];
  let onUtterance: ((text: string) => void) | null = null;
  let onError: ((message: string) => void) | null = null;

  async function transcribePendingAudio() {
    if (requestRunning) return;
    const pending = pendingAudio.shift();
    if (!pending) return;
    const { audio, run } = pending;
    if (!active || generation !== run || audio.size === 0) {
      void transcribePendingAudio();
      return;
    }
    requestRunning = true;
    const controller = new AbortController();
    activeRequest = controller;
    const timeout = window.setTimeout(() => controller.abort(), STT_TIMEOUT_MS);
    try {
      const form = new FormData();
      form.append("audio", audio, "wake-utterance.wav");
      if (useWakeHint) form.append("wake_hint", pronunciation);
      const response = await fetch(STT_ENDPOINT, {
        method: "POST",
        body: form,
        signal: controller.signal,
      });
      if (!response.ok) {
        const body: unknown = await response.json().catch(() => null);
        const message = typeof body === "object" && body !== null && "error" in body
          && typeof body.error === "string"
          ? body.error
          : `Wake-word transcription failed (HTTP ${response.status}).`;
        throw new Error(message);
      }
      const result: unknown = await response.json();
      if (typeof result === "object" && result !== null && "text" in result
        && typeof result.text === "string" && result.text.trim()
        && active && generation === run) {
        onUtterance?.(result.text.trim());
      }
    } catch (error) {
      if (active && generation === run) {
        const timedOut = error instanceof DOMException && error.name === "AbortError";
        onError?.(timedOut
          ? "Local Whisper wake-word transcription timed out."
          : error instanceof Error
            ? error.message
            : "Wake-word transcription failed.");
      }
    } finally {
      window.clearTimeout(timeout);
      if (activeRequest === controller) {
        activeRequest = null;
        requestRunning = false;
        wakeWindowSent = false;
      }
      void transcribePendingAudio();
    }
  }

  function resetUtterance() {
    speechFrames = [];
    speechSilenceMs = 0;
    speechStartedAt = 0;
  }

  function enqueueUtterance(run: number, sampleRate: number, frames: Float32Array[], durationMs: number) {
    if (!active || generation !== run || durationMs < MIN_SPEECH_MS || !frames.length) return;
    pendingAudio.push({ audio: encodeWav(frames, sampleRate), run });
    if (useWakeHint) {
      while (pendingAudio.length > 1) pendingAudio.shift();
    } else if (pendingAudio.length > 3) {
      pendingAudio.shift();
      onError?.("Wake-word transcription is busy; please try again.");
    }
    void transcribePendingAudio();
  }

  function finishUtterance(run: number, sampleRate: number) {
    if (!speechStartedAt || wakeWindowSent) {
      resetUtterance();
      return;
    }
    const durationMs = performance.now() - speechStartedAt;
    const utterance = speechFrames;
    resetUtterance();
    enqueueUtterance(run, sampleRate, utterance, durationMs);
  }

  function finishWakeWindow(run: number, sampleRate: number) {
    if (!speechStartedAt || wakeWindowSent) return;
    const durationMs = performance.now() - speechStartedAt;
    const utterance = speechFrames.slice();
    resetUtterance();
    if (durationMs < MIN_SPEECH_MS || !utterance.length) return;
    wakeWindowSent = true;
    enqueueUtterance(run, sampleRate, utterance, durationMs);
  }

  function stop() {
    generation += 1;
    active = false;
    pendingAudio.length = 0;
    const request = activeRequest;
    activeRequest = null;
    requestRunning = false;
    request?.abort();
    onUtterance = null;
    onError = null;
    if (processor) processor.onaudioprocess = null;
    processor?.disconnect();
    source?.disconnect();
    mutedOutput?.disconnect();
    void context?.close().catch((error: unknown) => {
      console.error("[coucou] could not close wake-word audio context", error);
    });
    stream?.getTracks().forEach((track) => track.stop());
    processor = null;
    source = null;
    mutedOutput = null;
    context = null;
    stream = null;
    resetUtterance();
    preRollFrames = [];
    preRollSamples = 0;
    wakeWindowSent = false;
  }

  return {
    async start(onText, onFailure, wakePronunciation, speechThreshold, withWakeHint = true) {
      if (active) return;
      if (!navigator.mediaDevices?.getUserMedia) {
        throw new Error("Always-listening microphone is not available in this window.");
      }
      await waitForSttReady();
      onUtterance = onText;
      onError = onFailure;
      pronunciation = wakePronunciation || "Hey Macha";
      useWakeHint = withWakeHint;
      threshold = Number.isFinite(speechThreshold)
        ? Math.max(0.002, Math.min(0.02, speechThreshold * 0.65))
        : DEFAULT_SPEECH_THRESHOLD;
      const run = ++generation;
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            channelCount: 1,
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true,
          },
        });
        if (generation !== run) {
          stream.getTracks().forEach((track) => track.stop());
          stream = null;
          return;
        }
        context = new AudioContext();
        await context.resume();
        if (generation !== run) {
          await context.close();
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          context = null;
          return;
        }
        source = context.createMediaStreamSource(stream);
        processor = context.createScriptProcessor(2048, 1, 1);
        mutedOutput = context.createGain();
        mutedOutput.gain.value = 0;
        const preRollLimit = Math.round(context.sampleRate * PRE_ROLL_MS / 1000);
        active = true;
        processor.onaudioprocess = (event) => {
          if (!active || generation !== run) return;
          if (useWakeHint && (wakeWindowSent || requestRunning)) return;
          const samples = event.inputBuffer.getChannelData(0);
          const copy = new Float32Array(samples);
          const frameMs = copy.length * 1000 / context!.sampleRate;
          const speaking = streamRms(copy) >= threshold;
          if (speaking) {
            if (!speechStartedAt) {
              speechStartedAt = performance.now();
              speechFrames = preRollFrames;
              preRollFrames = [];
              preRollSamples = 0;
            }
            speechFrames.push(copy);
            speechSilenceMs = 0;
          } else if (speechStartedAt) {
            speechFrames.push(copy);
            speechSilenceMs += frameMs;
            if (speechSilenceMs >= (useWakeHint ? WAKE_SILENCE_MS : SILENCE_MS)
              || performance.now() - speechStartedAt >= MAX_SPEECH_MS) {
              if (useWakeHint) finishWakeWindow(run, context!.sampleRate);
              else finishUtterance(run, context!.sampleRate);
            }
          } else {
            preRollFrames.push(copy);
            preRollSamples += copy.length;
            while (preRollSamples > preRollLimit && preRollFrames.length) {
              preRollSamples -= preRollFrames.shift()!.length;
            }
          }
          if (speechStartedAt && performance.now() - speechStartedAt >= MAX_SPEECH_MS) {
            if (useWakeHint) finishWakeWindow(run, context!.sampleRate);
            else finishUtterance(run, context!.sampleRate);
          }
        };
        source.connect(processor);
        processor.connect(mutedOutput);
        mutedOutput.connect(context.destination);
      } catch (error) {
        stop();
        throw error;
      }
    },
    stop,
    isActive: () => active,
  };
}
