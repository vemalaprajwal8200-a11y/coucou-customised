// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import {
  Bridge,
  IS_TAURI,
  onEvent,
  type AutomationAction,
  type ChatContext,
  type ChatReply,
} from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import * as TTS from "../core/tts";
import { createWakeWordListener, extractWakeCommand, isWakeLeadOnly } from "../core/wakeWord";
import type { ViewHost } from "./views";

let nextId = 1;

const STT_ENDPOINT = "http://127.0.0.1:5005/transcribe";
const STT_TIMEOUT_MS = 10_000;
const VOICE_SILENCE_MS = 450;
const VOICE_VAD_POLL_MS = 50;
const VOICE_NOISE_CALIBRATION_MS = 300;
const VOICE_NO_SPEECH_TIMEOUT_MS = 10_000;
const VOICE_MAX_RECORDING_MS = 30_000;
type VoiceState = "idle" | "wake" | "listening" | "transcribing" | "speaking";

function newRequestId(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

async function copyToClipboard(value: string, button: HTMLButtonElement): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText(value);
      } catch {
        copyWithLegacyCommand(value);
      }
    } else {
      copyWithLegacyCommand(value);
    }
    button.textContent = "Copied";
  } catch (error) {
    console.error("[coucou] could not copy chat text", error);
    button.textContent = "Copy failed";
  }
  button.disabled = true;
  window.setTimeout(() => {
    button.textContent = button.dataset.label ?? "Copy";
    button.disabled = false;
  }, 1400);
}

function copyWithLegacyCommand(value: string): void {
  const field = h("textarea", {
    readonly: true,
    style: "position:fixed;left:-9999px;top:0",
  }) as HTMLTextAreaElement;
  field.value = value;
  document.body.append(field);
  let copied = false;
  try {
    field.focus();
    field.select();
    copied = document.execCommand("copy");
  } finally {
    field.remove();
  }
  if (!copied) throw new Error("Clipboard access is unavailable.");
}

function copyButton(label: string, value: string): HTMLButtonElement {
  const button = h("button", {
    class: "chat-copy-button",
    text: label,
    title: label,
    "aria-label": label,
  }) as HTMLButtonElement;
  button.dataset.label = label;
  button.addEventListener("click", () => void copyToClipboard(value, button));
  return button;
}

function renderReplyContent(reply: HTMLElement, text: string): void {
  const fencedCode = /```([^\n`]*)\r?\n([\s\S]*?)```/g;
  let cursor = 0;
  let match: RegExpExecArray | null;
  while ((match = fencedCode.exec(text)) !== null) {
    if (match.index > cursor) {
      reply.append(h("div", { class: "reply-text", text: text.slice(cursor, match.index) }));
    }
    const language = match[1].trim();
    const code = match[2].replace(/\r?\n$/, "");
    const pre = h("pre", { class: "chat-code" });
    pre.append(h("code", { text: code }));
    const block = h(
      "div",
      { class: "chat-code-block" },
      h("div", { class: "chat-code-header" },
        h("span", { class: "chat-code-language", text: language || "Code" }),
        copyButton("Copy code", code),
      ),
      pre,
    );
    reply.append(block);
    cursor = fencedCode.lastIndex;
  }
  if (cursor < text.length || cursor === 0) {
    reply.append(h("div", { class: "reply-text", text: text.slice(cursor) }));
  }
}

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  const reply = h("div", { class: "reply" });
  renderReplyContent(reply, message.content);
  reply.append(
    h("div", { class: "chat-reply-footer" },
      message.fallbackNotice
        ? h("span", { class: "chat-fallback-notice", text: message.fallbackNotice })
        : h("span", {}),
      message.model
        ? h("span", { class: "chat-model-label", text: `Model used: ${message.model}` })
        : h("span", {}),
      copyButton("Copy response", message.content),
    ),
  );
  return h("div", { class: "chat-row" }, reply);
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

function actionDescription(action: AutomationAction): string {
  switch (action.name) {
    case "open_app": return `open ${String(action.arguments.appName ?? "the app")}`;
    case "list_directory": return "list files in a folder";
    case "read_file": return "read a file (contents go to the selected chat provider)";
    case "open_path": return "open an item in the file manager";
    case "create_directory": return "create a folder";
    case "create_file": return "create a new file";
    case "move_file": return "move a file";
    case "write_file": return "replace all contents of a file";
    case "erase_file_content": return "clear all contents of a file";
    default: return "perform a local file action";
  }
}

function isOpenAction(
  action: AutomationAction | null,
): action is AutomationAction & { name: "open_app" | "open_path" } {
  return action?.name === "open_app" || action?.name === "open_path";
}

interface AppChoice {
  id: string;
  label: string;
}

function appChoices(action: AutomationAction | null): AppChoice[] {
  const raw = action?.arguments.appChoices;
  if (!Array.isArray(raw)) return [];
  return raw.filter((choice): choice is AppChoice =>
    typeof choice === "object" && choice !== null &&
    "id" in choice && typeof choice.id === "string" &&
    "label" in choice && typeof choice.label === "string",
  );
}

function launchTarget(action: AutomationAction | null): AppChoice | null {
  const target = action?.arguments.launchTarget;
  if (typeof target !== "object" || target === null || !("id" in target) ||
    typeof target.id !== "string" || !("label" in target) || typeof target.label !== "string") {
    return null;
  }
  return { id: target.id, label: target.label };
}

function openActionQuestion(action: AutomationAction, selectedLabel?: string): string {
  const choices = appChoices(action);
  const target = selectedLabel
    ?? launchTarget(action)?.label
    ?? (action.name === "open_app"
      ? String(action.arguments.appName ?? "this application")
      : String(action.arguments.path ?? "this file or folder"));
  if (action.name === "open_app" && choices.length > 1 && !selectedLabel) {
    return "Which of these exact-name applications should I open?";
  }
  const description = action.name === "open_app" ? "application" : "file or folder";
  return `Are you sure you want to open the ${description} ${target}?`;
}

function normalizeSpokenName(value: string): string {
  return value.normalize("NFKD").toLowerCase().replace(/[^a-z0-9]+/g, " ").trim();
}

function spokenConfirmation(text: string): boolean | null {
  const normalized = text.trim().toLowerCase().replace(/^[\s.,!?;:]+|[\s.,!?;:]+$/g, "");
  if (/^(?:yes|yeah|yep|sure|okay|ok|affirmative)\b/.test(normalized)) return true;
  if (/^(?:no|nope|nah|negative)\b/.test(normalized)) return false;
  return null;
}

function spokenAppChoice(text: string, choices: AppChoice[]): AppChoice | null {
  const words: Record<string, number> = {
    first: 1, one: 1, second: 2, two: 2, third: 3, three: 3,
    fourth: 4, four: 4, fifth: 5, five: 5, sixth: 6, six: 6,
  };
  const ordinal = text.trim().toLowerCase().match(/^(?:option\s+)?(first|one|second|two|third|three|fourth|four|fifth|five|sixth|six|[1-9])(?:\s+one)?[.!?]?$/);
  const index = ordinal
    ? Number(ordinal[1]) || words[ordinal[1]]
    : undefined;
  if (index && choices[index - 1]) return choices[index - 1];
  const spoken = normalizeSpokenName(text);
  return choices.find((choice) => {
    const label = normalizeSpokenName(choice.label);
    return label.length > 0 && spoken === label;
  }) ?? null;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  let sending = false;
  let activeRequestId: string | null = null;
  let cancelRequested = false;
  let pendingAction: AutomationAction | null = null;
  let pendingConversationId: string | null = null;
  let historyOpen = false;
  let renderedKey = "";
  let voiceState: VoiceState = "idle";
  let mediaStream: MediaStream | null = null;
  let recorder: MediaRecorder | null = null;
  let voiceMonitorContext: AudioContext | null = null;
  let voiceMonitorTimer: number | null = null;
  let voiceRecordingStartedAt = 0;
  let voiceSpeechStartedAt = 0;
  let voiceLastSpeechAt = 0;
  let audioChunks: Blob[] = [];
  let voiceNotice = "";
  let voiceNoticeIsError = false;
  const wakeWord = createWakeWordListener();
  let wakeWordReady = false;
  let wakeWordStarting = false;
  let lastWakeWordEnabled = State.settings.wakeWordEnabled;
  let lastWakePronunciation = State.settings.wakeWordPronunciation;
  let lastWakeThreshold = State.settings.wakeWordThreshold;
  let wakeCalibrationActive = false;
  let pendingWakeLead: string | null = null;
  let wakeLeadTimer: number | null = null;
  let suppressWakeRestart = false;
  let wakeAckInProgress = false;
  let actionConfirmationActive = false;
  let actionConfirmationProcessing = false;
  let actionConfirmationStage: "choose" | "confirm" = "confirm";
  let selectedAppChoiceId: string | null = null;
  let wakeConversationHeld = false;

  const historyToggle = h("button", {
    class: "chat-action",
    text: "Past conversations",
    onclick: () => {
      historyOpen = !historyOpen;
      State.notify();
    },
  });
  const newChat = h("button", {
    class: "chat-action primary",
    text: "New conversation",
    onclick: () => {
      if (sending || pendingAction) return;
      State.startConversation();
      historyOpen = false;
      onHeightChange();
      input.focus();
    },
  });
  const toolbar = h("div", { class: "chat-toolbar" }, historyToggle, newChat);
  const historyList = h("div", { class: "conversation-list" });
  historyList.hidden = true;
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const sendIcon = svg(ICONS.arrowUp, 11);
  const send = h("button", { class: "send-btn", title: "Send" }, sendIcon);
  const voiceButton = h(
    "button",
    { class: "voice-btn", title: "Start voice input", "aria-label": "Start voice input" },
    h("i", { class: "voice-indicator" }),
    h("span", { text: "Voice" }),
  );
  const stopSpeechButton = h("button", {
    class: "voice-stop-btn",
    text: "Stop",
    title: "Stop speaking",
    "aria-label": "Stop speaking",
  }) as HTMLButtonElement;
  stopSpeechButton.hidden = true;
  stopSpeechButton.addEventListener("click", () => void TTS.stop());
  const voiceStatus = h("span", { class: "voice-status", "aria-live": "polite" });
  const bar = h("div", { class: "chat-bar" }, input, voiceStatus, voiceButton, stopSpeechButton, send);

  function updateVoiceUi() {
    const labels: Record<VoiceState, string> = {
      idle: "Voice",
      wake: "Hey Macha",
      listening: "Listening",
      transcribing: "Transcribing",
      speaking: "Speaking",
    };
    voiceButton.classList.toggle("listening", voiceState === "listening");
    voiceButton.classList.toggle("transcribing", voiceState === "transcribing");
    voiceButton.classList.toggle("speaking", voiceState === "speaking");
    voiceButton.classList.toggle("wake-active", voiceState === "wake");
    voiceButton.title = voiceState === "listening"
      ? "Stop recording"
      : voiceState === "wake"
        ? 'Listening for "Hey Macha" — click to speak now'
        : "Start voice input";
    voiceButton.setAttribute("aria-label", voiceButton.title);
    voiceButton.querySelector("span")!.textContent = labels[voiceState];
    stopSpeechButton.hidden = voiceState !== "speaking";
    voiceStatus.textContent = voiceNotice;
    voiceStatus.classList.toggle("error", voiceNoticeIsError);
  }

  function setVoiceState(next: VoiceState, notice = "", isError = false) {
    voiceState = next;
    voiceNotice = notice;
    voiceNoticeIsError = isError;
    updateVoiceUi();
  }

  function stopWakeWord() {
    wakeWord.stop();
    pendingWakeLead = null;
    if (wakeLeadTimer !== null) {
      window.clearTimeout(wakeLeadTimer);
      wakeLeadTimer = null;
    }
  }

  function holdWakeConversation() {
    wakeConversationHeld = true;
    window.dispatchEvent(new Event("coucou-wake-word"));
  }

  function releaseWakeConversation() {
    if (!wakeConversationHeld) return;
    wakeConversationHeld = false;
    window.dispatchEvent(new Event("coucou-wake-word-complete"));
  }

  async function reconcileWakeWord() {
    if (!wakeWordReady || !State.settings.wakeWordEnabled || !IS_TAURI || sending || wakeCalibrationActive
      || !!pendingAction || actionConfirmationActive
      || voiceState === "listening" || voiceState === "transcribing"
      || voiceState === "speaking" || TTS.isSpeaking()) {
      if (wakeWord.isActive()) wakeWord.stop();
      if (voiceState === "wake") setVoiceState("idle");
      return;
    }
    if (wakeWord.isActive() || wakeWordStarting) return;
    wakeWordStarting = true;
    try {
      await wakeWord.start(
        (text) => void onWakeUtterance(text),
        (message) => {
          console.error("[coucou] Hey Macha wake-word listener error", message);
          setVoiceState(wakeWord.isActive() ? "wake" : "idle", message, true);
        },
        State.settings.wakeWordPronunciation,
        State.settings.wakeWordThreshold,
      );
      if (wakeWord.isActive()) setVoiceState("wake", 'Say "Hey Macha" to ask a question.');
    } catch (error) {
      const name = error instanceof DOMException ? error.name : "";
      const message = name === "NotAllowedError" || name === "SecurityError"
        ? 'Microphone permission denied. Allow microphone access to use "Hey Macha".'
        : error instanceof Error
          ? error.message
          : "Could not start the Hey Macha wake-word listener.";
      setVoiceState("idle", message, true);
      console.error("[coucou] could not start Macha wake-word listener", error);
    } finally {
      wakeWordStarting = false;
    }
  }

  async function onWakeUtterance(transcript: string) {
    let wakeText = transcript;
    if (pendingWakeLead) {
      wakeText = `${pendingWakeLead} ${transcript}`;
      pendingWakeLead = null;
      if (wakeLeadTimer !== null) {
        window.clearTimeout(wakeLeadTimer);
        wakeLeadTimer = null;
      }
    } else if (isWakeLeadOnly(transcript, State.settings.wakeWordPronunciation)) {
      pendingWakeLead = transcript.trim();
      wakeLeadTimer = window.setTimeout(() => {
        pendingWakeLead = null;
        wakeLeadTimer = null;
      }, 4_000);
      return;
    }

    const command = extractWakeCommand(wakeText, State.settings.wakeWordPronunciation);
    if (command === null) return;

    stopWakeWord();
    holdWakeConversation();
    suppressWakeRestart = true;
    wakeAckInProgress = true;
    Sound.play("blip");
    setVoiceState("listening", "Yes Boss — I'm listening.");
    try {
      await TTS.speak("Yes Boss");
    } catch (error: unknown) {
      console.error("[coucou] could not speak wake-word acknowledgement", error);
    } finally {
      wakeAckInProgress = false;
    }
    if (!command) {
      await startListening();
      return;
    }

    suppressWakeRestart = false;
    try {
      await Bridge.setVoiceActive(true);
      input.value = command;
      setVoiceState("transcribing", "Sending your message…");
      await submit(command, true, wakeConversationHeld);
    } catch (error) {
      await releaseVoiceSession();
      setVoiceState(
        "idle",
        error instanceof Error ? error.message : "Could not send the wake-word command.",
        true,
      );
    }
  }

  window.addEventListener("coucou-settings-ready", () => {
    wakeWordReady = true;
    void reconcileWakeWord();
  }, { once: true });
  State.subscribe(() => {
    const phraseChanged = lastWakePronunciation !== State.settings.wakeWordPronunciation
      || lastWakeThreshold !== State.settings.wakeWordThreshold;
    if (lastWakeWordEnabled === State.settings.wakeWordEnabled && !phraseChanged) return;
    lastWakeWordEnabled = State.settings.wakeWordEnabled;
    lastWakePronunciation = State.settings.wakeWordPronunciation;
    lastWakeThreshold = State.settings.wakeWordThreshold;
    if (phraseChanged && wakeWord.isActive()) wakeWord.stop();
    if (!lastWakeWordEnabled) {
      stopWakeWord();
      setVoiceState("idle");
    } else {
      void reconcileWakeWord();
    }
  });
  void onEvent<boolean>("wake-calibration", (active) => {
    wakeCalibrationActive = active;
    if (active) {
      stopWakeWord();
      setVoiceState("idle", "Recording your wake phrase…");
    } else {
      setVoiceState("idle");
      void reconcileWakeWord();
    }
  });

  TTS.onStateChange((speaking, error) => {
    if (speaking) {
      stopWakeWord();
      if (wakeAckInProgress) {
        setVoiceState("listening", "Yes Boss — I'm listening.");
      } else {
        setVoiceState("speaking", error ?? "", !!error);
      }
      void Bridge.setVoiceActive(true).catch((cause: unknown) => {
        console.error("[coucou] could not hold island visible for speech", cause);
      });
    } else if (wakeAckInProgress) {
      setVoiceState("listening", "Yes Boss — I'm listening.");
    } else if (voiceState === "speaking") {
      setVoiceState("idle", error ?? "", !!error);
      void Bridge.setVoiceActive(false).catch((cause: unknown) => {
        console.error("[coucou] could not release speech auto-hide hold", cause);
      });
      if (!suppressWakeRestart) void reconcileWakeWord();
    } else if (error) {
      setVoiceState(voiceState, error, true);
    }
  });

  function stopMediaTracks() {
    if (voiceMonitorTimer !== null) {
      window.clearInterval(voiceMonitorTimer);
      voiceMonitorTimer = null;
    }
    const monitorContext = voiceMonitorContext;
    voiceMonitorContext = null;
    if (monitorContext && monitorContext.state !== "closed") {
      void monitorContext.close().catch((error: unknown) => {
        console.error("[coucou] could not close voice activity monitor", error);
      });
    }
    mediaStream?.getTracks().forEach((track) => track.stop());
    mediaStream = null;
    recorder = null;
    voiceSpeechStartedAt = 0;
    voiceLastSpeechAt = 0;
  }

  function monitorVoiceActivity(analyser: AnalyserNode) {
    const samples = new Float32Array(analyser.fftSize);
    const noiseSamples: number[] = [];
    let noiseFloor = 0;
    const baseThreshold = Math.max(
      0.0025,
      Math.min(0.012, State.settings.wakeWordThreshold * 0.3),
    );
    let speechThreshold = baseThreshold;
    const check = () => {
      if (!recorder || recorder.state !== "recording") {
        stopListening();
        return;
      }

      analyser.getFloatTimeDomainData(samples);
      let sumSquares = 0;
      for (const sample of samples) sumSquares += sample * sample;
      const level = Math.sqrt(sumSquares / samples.length);
      const now = performance.now();

      if (now - voiceRecordingStartedAt <= VOICE_NOISE_CALIBRATION_MS) {
        noiseSamples.push(level);
      } else {
        if (noiseSamples.length > 0) {
          noiseSamples.sort((a, b) => a - b);
          noiseFloor = noiseSamples[Math.floor(noiseSamples.length * 0.2)];
          speechThreshold = Math.max(baseThreshold, Math.min(0.02, noiseFloor * 1.8));
          noiseSamples.length = 0;
        }

        if (level >= speechThreshold) {
          if (!voiceSpeechStartedAt) voiceSpeechStartedAt = now;
          voiceLastSpeechAt = now;
        } else if (!voiceSpeechStartedAt) {
          noiseFloor = noiseFloor * 0.9 + level * 0.1;
          speechThreshold = Math.max(baseThreshold, Math.min(0.02, noiseFloor * 1.8));
        }
      }

      const recordingDuration = now - voiceRecordingStartedAt;
      const speechDuration = voiceSpeechStartedAt ? now - voiceSpeechStartedAt : 0;
      if (recordingDuration >= VOICE_MAX_RECORDING_MS
        || (!voiceSpeechStartedAt && recordingDuration >= VOICE_NO_SPEECH_TIMEOUT_MS)
        || (speechDuration >= 250 && now - voiceLastSpeechAt >= VOICE_SILENCE_MS)) {
        stopListening();
      }
    };
    voiceMonitorTimer = window.setInterval(check, VOICE_VAD_POLL_MS);
  }

  async function releaseVoiceSession() {
    if (voiceState !== "idle") setVoiceState("idle");
    try {
      await Bridge.setVoiceActive(false);
    } catch (error) {
      console.error("[coucou] could not release voice auto-hide hold", error);
    }
    releaseWakeConversation();
    if (!suppressWakeRestart) void reconcileWakeWord();
  }

  async function listenForActionConfirmation() {
    if (!actionConfirmationActive || !isOpenAction(pendingAction)) return;
    try {
      await wakeWord.start(
        (text) => void onActionConfirmationTranscript(text),
        (message) => {
          console.error("[coucou] voice confirmation listener error", message);
          setVoiceState("idle", message, true);
        },
        "",
        State.settings.wakeWordThreshold,
        false,
      );
      if (wakeWord.isActive()) {
        setVoiceState(
          "listening",
          actionConfirmationStage === "choose"
            ? "Say the full app name or its option number."
            : 'Say "yes" to open, or "no" to cancel.',
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "Could not start voice confirmation.";
      setVoiceState("idle", `${message} Use the Yes or No button.`, true);
      console.error("[coucou] could not start voice confirmation listener", error);
    }
  }

  async function askToOpenAction(tryAgain = false) {
    if (!isOpenAction(pendingAction)) return;
    const action = pendingAction;
    const choices = appChoices(action);
    const selected = choices.find((choice) => choice.id === selectedAppChoiceId);
    try {
      const prompt = action.name === "open_app" && choices.length > 1 && actionConfirmationStage === "choose"
        ? tryAgain
          ? `Please say the full name of the app, or say its option number. ${choices.map((choice, index) => `Option ${index + 1}: ${choice.label}.`).join(" ")}`
          : `I found more than one installed app with that full name. Which one should I open? ${choices.map((choice, index) => `Option ${index + 1}: ${choice.label}.`).join(" ")} Say the full name or option number.`
        : tryAgain
          ? 'Please say "yes" to open it or "no" to cancel.'
          : `${openActionQuestion(action, selected?.label)} Say "yes" to open it or "no" to cancel.`;
      await TTS.speak(prompt);
    } catch (error) {
      console.error("[coucou] could not speak the open confirmation", error);
      setVoiceState("idle", "Could not speak the confirmation. Use the Yes or No button.", true);
    }
    await listenForActionConfirmation();
  }

  async function beginActionConfirmation() {
    if (!isOpenAction(pendingAction) || actionConfirmationActive) return;
    actionConfirmationActive = true;
    actionConfirmationProcessing = false;
    selectedAppChoiceId = null;
    actionConfirmationStage = appChoices(pendingAction).length > 1 ? "choose" : "confirm";
    State.notify();
    stopWakeWord();
    holdWakeConversation();
    await askToOpenAction();
  }

  async function onActionConfirmationTranscript(text: string) {
    if (!actionConfirmationActive || actionConfirmationProcessing) return;
    actionConfirmationProcessing = true;
    wakeWord.stop();
    if (actionConfirmationStage === "choose" && isOpenAction(pendingAction)) {
      const choice = spokenAppChoice(text, appChoices(pendingAction));
      if (!choice) {
        actionConfirmationProcessing = false;
        setVoiceState("speaking");
        await askToOpenAction(true);
        return;
      }
      selectedAppChoiceId = choice.id;
      actionConfirmationStage = "confirm";
      State.notify();
      setVoiceState("speaking");
      await askToOpenAction();
      actionConfirmationProcessing = false;
      return;
    }
    const approved = spokenConfirmation(text);
    if (approved !== null) {
      actionConfirmationActive = false;
      setVoiceState("transcribing", approved ? "Opening as requested…" : "Cancelling the open request…");
      await answerAction(approved, true);
      return;
    }
    setVoiceState("speaking");
    await askToOpenAction(true);
    actionConfirmationProcessing = false;
  }

  async function startListening() {
    if (voiceState === "transcribing" || (sending && voiceState !== "speaking")) return;
    stopWakeWord();
    voiceNotice = "";
    voiceNoticeIsError = false;
    suppressWakeRestart = true;
    try {
      await TTS.stop();
    } catch (error) {
      suppressWakeRestart = false;
      console.error("[coucou] could not stop speech before microphone recording", error);
      setVoiceState("speaking", "Could not stop speech before recording.", true);
      return;
    }
    setVoiceState("listening", "I'm listening — speak now.");
    try {
      await Bridge.setVoiceActive(true);
      if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
        throw new Error("Microphone recording is not available in this window.");
      }
      mediaStream = await navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        },
      });
      voiceMonitorContext = new AudioContext();
      await voiceMonitorContext.resume();
      const source = voiceMonitorContext.createMediaStreamSource(mediaStream);
      const analyser = voiceMonitorContext.createAnalyser();
      const mutedOutput = voiceMonitorContext.createGain();
      analyser.fftSize = 2048;
      mutedOutput.gain.value = 0;
      source.connect(analyser);
      analyser.connect(mutedOutput);
      mutedOutput.connect(voiceMonitorContext.destination);
      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus"
        : "audio/webm";
      recorder = new MediaRecorder(mediaStream, { mimeType });
      voiceRecordingStartedAt = performance.now();
      voiceSpeechStartedAt = 0;
      voiceLastSpeechAt = voiceRecordingStartedAt;
      audioChunks = [];
      recorder.addEventListener("dataavailable", (event) => {
        if (event.data.size > 0) audioChunks.push(event.data);
      });
      recorder.addEventListener("error", () => {
        stopMediaTracks();
        void releaseVoiceSession();
        setVoiceState("idle", "Microphone recording failed.", true);
      }, { once: true });
      recorder.addEventListener("stop", () => {
        const audio = new Blob(audioChunks, { type: mimeType });
        audioChunks = [];
        stopMediaTracks();
        void transcribe(audio);
      }, { once: true });
      recorder.start();
      monitorVoiceActivity(analyser);
      input.blur();
    } catch (error) {
      stopMediaTracks();
      const name = error instanceof DOMException ? error.name : "";
      const message = name === "NotAllowedError" || name === "SecurityError"
        ? "Microphone permission denied."
        : name === "NotFoundError" || name === "DevicesNotFoundError"
          ? "No microphone found."
          : error instanceof Error
            ? error.message
            : "Could not start microphone recording.";
      await releaseVoiceSession();
      setVoiceState("idle", message, true);
    } finally {
      suppressWakeRestart = false;
    }
  }

  function stopListening() {
    const activeRecorder = recorder;
    if (!activeRecorder || activeRecorder.state !== "recording") {
      if (voiceState === "listening") {
        stopMediaTracks();
        void releaseVoiceSession();
      }
      return;
    }
    setVoiceState("transcribing", "Finishing speech…");
    try {
      activeRecorder.stop();
    } catch (error) {
      console.error("[coucou] could not stop microphone recording", error);
      stopMediaTracks();
      void releaseVoiceSession();
      setVoiceState("idle", "Could not finish microphone recording.", true);
    }
  }

  async function transcribe(audio: Blob) {
    if (audio.size === 0) {
      await releaseVoiceSession();
      setVoiceState("idle", "No speech detected.");
      return;
    }
    const form = new FormData();
    form.append("audio", audio, "recording.webm");
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), STT_TIMEOUT_MS);
    try {
      const response = await fetch(STT_ENDPOINT, {
        method: "POST",
        body: form,
        signal: controller.signal,
      });
      if (!response.ok) {
        if (response.status >= 500) throw new Error("Voice server not running");
        const body: unknown = await response.json().catch(() => null);
        const message = typeof body === "object" && body !== null && "error" in body
          && typeof body.error === "string"
          ? body.error
          : `Voice transcription failed (HTTP ${response.status}).`;
        throw new Error(message);
      }
      const body: unknown = await response.json();
      const transcript = typeof body === "object" && body !== null && "text" in body
        && typeof body.text === "string"
        ? body.text.trim()
        : "";
      const confidence = typeof body === "object" && body !== null && "confidence" in body
        && typeof body.confidence === "number"
        ? body.confidence
        : null;
      if (!transcript || confidence === null) {
        await releaseVoiceSession();
        setVoiceState("idle", "No speech detected. Please try again.");
        return;
      }
      if (confidence < -8) {
        await releaseVoiceSession();
        setVoiceState(
          "idle",
          "I could not understand that clearly. Please try again.",
          true,
        );
        return;
      }
      input.value = transcript;
      setVoiceState("transcribing", "Sending your message…");
      await submit(transcript, true, wakeConversationHeld);
      if (!pendingAction && voiceState !== "speaking") {
        await releaseVoiceSession();
      }
    } catch (error) {
      const message = error instanceof DOMException && error.name === "AbortError"
        ? "Voice transcription timed out."
        : error instanceof TypeError
          ? "Voice server not running"
          : error instanceof Error
            ? error.message
            : "Voice transcription failed.";
      await releaseVoiceSession();
      setVoiceState("idle", message, true);
    } finally {
      window.clearTimeout(timeout);
    }
  }

  async function toggleVoice() {
    if (actionConfirmationActive) return;
    if (voiceState === "listening") {
      stopListening();
    } else if (voiceState === "speaking" || voiceState === "wake") {
      await startListening();
    } else if (voiceState === "idle") {
      await startListening();
    }
  }

  voiceButton.addEventListener("click", () => void toggleVoice());
  void onEvent<null>("voice-hotkey", () => void toggleVoice());

  const el = h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "card wash chat-card" },
      h("div", { class: "chat-body" }, toolbar, historyList, chipRow, log, bar),
    ),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  async function submit(
    queryOverride?: string,
    voiceRequest = false,
    wakeInitiated = false,
    acknowledgement?: Promise<void>,
  ) {
    const query = (queryOverride ?? input.value).trim();
    if (!query || sending || pendingAction) {
      if (voiceRequest) await releaseVoiceSession();
      if (wakeInitiated) releaseWakeConversation();
      return;
    }
    const conversationId = State.activeConversationId ?? State.startConversation();
    const requestId = newRequestId();
    activeRequestId = requestId;
    cancelRequested = false;
    const history = State.chatHistory.map((message) => ({ ...message }));
    input.value = "";
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.saveActiveConversation();
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null = file ? { kind: "file", name: file.name, path: file.path } : null;
    const sharedContext = State.sharedConversationContext(conversationId);

    try {
      const reply = await Bridge.chatSend(
        conversationId,
        history,
        query,
        context,
        sharedContext,
        requestId,
        false,
        voiceRequest,
      );
      await showReply(reply, conversationId, voiceRequest, acknowledgement);
    } catch (err) {
      const lastMessage = State.chatHistory[State.chatHistory.length - 1];
      if (lastMessage?.role === "user" && lastMessage.content === query) {
        State.chatHistory.pop();
        State.saveActiveConversation();
      }
      if (!cancelRequested) {
        State.stateOverride = null;
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        State.view = "note";
        Sound.play("error");
      }
      if (voiceRequest) await releaseVoiceSession();
    } finally {
      sending = false;
      activeRequestId = null;
      cancelRequested = false;
      State.notify();
      onHeightChange();
      input.focus();
      if (voiceState === "idle") void reconcileWakeWord();
      if (isOpenAction(pendingAction)) {
        void beginActionConfirmation();
      } else if (wakeInitiated) {
        releaseWakeConversation();
      }
    }
  }

  async function showReply(
    reply: ChatReply,
    conversationId: string,
    voiceRequest = false,
    acknowledgement?: Promise<void>,
  ): Promise<void> {
    pendingAction = reply.action;
    pendingConversationId = reply.action ? conversationId : null;
    selectedAppChoiceId = null;
    actionConfirmationActive = false;
    actionConfirmationProcessing = false;
    actionConfirmationStage = "confirm";
    const content = reply.text || (reply.action
      ? `Mochi is asking to ${actionDescription(reply.action)}.`
      : "");
    if (content) {
      State.chatHistory.push({
        id: nextId++,
        role: "assistant",
        content,
        model: reply.model,
        fallbackNotice: reply.fallbackNotice ?? undefined,
      });
      State.saveActiveConversation();
    }
    State.stateOverride = null;
    Sound.play(reply.action ? "send" : "finish");
    State.notify();
    onHeightChange();
    if (acknowledgement) await acknowledgement;
    const shouldSpeak = State.settings.speakRepliesMode === "always"
      || (State.settings.speakRepliesMode === "voiceOnly" && voiceRequest);
    if (shouldSpeak && reply.text.trim()) {
      const assistantMessage = content
        ? State.chatHistory[State.chatHistory.length - 1]
        : undefined;
      if (assistantMessage?.role === "assistant") {
        assistantMessage.content = "";
        State.notify();
        const speech = TTS.speak(reply.text);
        await Promise.all([
          speech,
          revealReplyWhileSpeaking(assistantMessage, content, speech),
        ]);
        assistantMessage.content = content;
        State.saveActiveConversation();
        State.notify();
        onHeightChange();
      } else {
        await TTS.speak(reply.text);
      }
    } else if (voiceRequest && !reply.action) {
      void releaseVoiceSession();
    }
  }

  async function revealReplyWhileSpeaking(
    message: ChatMessage,
    text: string,
    speech: Promise<void>,
  ): Promise<void> {
    let speechFinished = false;
    void speech.then(
      () => { speechFinished = true; },
      () => { speechFinished = true; },
    );
    const delay = (milliseconds: number) =>
      new Promise<void>((resolve) => window.setTimeout(resolve, milliseconds));
    while (!TTS.isSpeaking() && !speechFinished) await delay(20);

    const words = text.match(/\S+\s*/gu) ?? [];
    let displayed = "";
    for (const word of words) {
      if (speechFinished || !TTS.isSpeaking()) break;
      displayed += word;
      message.content = displayed;
      State.notify();
      onHeightChange();
      const pause = /[.!?]["')\]]*\s*$/.test(word) ? 420 : 320;
      await delay(pause / State.settings.ttsRate);
    }
    message.content = text;
    State.notify();
    onHeightChange();
  }

  async function answerAction(approved: boolean, voiceConfirmed = false) {
    const conversationId = pendingConversationId;
    if (!conversationId || sending || !pendingAction) return;
    const opening = isOpenAction(pendingAction);
    actionConfirmationActive = false;
    actionConfirmationProcessing = false;
    stopWakeWord();
    if (opening && TTS.isSpeaking()) {
      try {
        await TTS.stop();
      } catch (error) {
        console.error("[coucou] could not stop the open confirmation speech", error);
      }
    }
    sending = true;
    const requestId = newRequestId();
    activeRequestId = requestId;
    cancelRequested = false;
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();
    try {
      const selectedApp = approved && pendingAction?.name === "open_app"
        ? selectedAppChoiceId ?? launchTarget(pendingAction)?.id
        : undefined;
      const reply = await Bridge.chatAction(conversationId, approved, requestId, selectedApp);
      await showReply(reply, conversationId, voiceConfirmed || wakeConversationHeld);
    } catch (err) {
      if (!cancelRequested) {
        pendingAction = null;
        pendingConversationId = null;
        State.stateOverride = null;
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        State.view = "note";
        Sound.play("error");
      }
    } finally {
      sending = false;
      activeRequestId = null;
      cancelRequested = false;
      State.notify();
      onHeightChange();
      if (isOpenAction(pendingAction)) {
        void beginActionConfirmation();
      } else {
        releaseWakeConversation();
      }
    }
  }

  function selectAppChoice(choice: AppChoice) {
    if (!actionConfirmationActive || !appChoices(pendingAction).some((item) => item.id === choice.id)) return;
    selectedAppChoiceId = choice.id;
    actionConfirmationStage = "confirm";
    actionConfirmationProcessing = true;
    wakeWord.stop();
    State.notify();
    setVoiceState("speaking");
    void askToOpenAction().finally(() => {
      actionConfirmationProcessing = false;
    });
  }

  function actionCard(action: AutomationAction): HTMLElement {
    const args = action.arguments;
    const path = typeof args.path === "string" ? args.path : "";
    const destination = typeof args.destination === "string" ? args.destination : "";
    const content = typeof args.content === "string" ? args.content : "";
    const appName = typeof args.appName === "string" ? args.appName : "";
    const destructive = action.name === "write_file" || action.name === "erase_file_content";
    const approvalText = isOpenAction(action)
      ? "Yes, open it"
      : action.name === "write_file"
        ? "Replace contents"
        : action.name === "erase_file_content"
          ? "Clear contents"
          : "Approve";
    const choices = appChoices(action);
    const choiceSelected = choices.some((choice) => choice.id === selectedAppChoiceId);
    const approve = h("button", {
      class: destructive ? "automation-deny" : "automation-approve",
      text: approvalText,
      disabled: sending || (choices.length > 1 && !choiceSelected),
      onclick: () => void answerAction(true),
    });
    const deny = h("button", {
      class: "automation-deny",
      text: isOpenAction(action) ? "No, cancel" : "Deny",
      disabled: sending,
      onclick: () => void answerAction(false),
    });
    const card = h(
      "div",
      { class: "automation-card" },
      h("strong", { text: isOpenAction(action) ? openActionQuestion(action) : actionDescription(action) }),
      h("span", {
        class: "hint",
        text: action.name === "read_file"
          ? "If approved, file contents are sent to the selected chat provider. Nothing happens until you approve."
          : action.name === "open_app"
            ? choices.length > 1 && actionConfirmationStage === "choose"
              ? "Choose the exact-name app match before confirming the launch."
              : 'Say "yes" to open this app or "no" to cancel. The buttons also work.'
                : action.name === "open_path"
                  ? 'Say "yes" to open this file or folder or "no" to cancel. The buttons also work.'
            : destructive
              ? "This changes existing file contents. A backup is made first, and nothing changes until you approve."
          : "This action is limited to your authorized folders. Nothing happens until you approve.",
      }),
    );
    if (appName) card.append(h("code", { class: "automation-path", text: appName }));
    if (path) card.append(h("code", { class: "automation-path", text: path }));
    if (choices.length > 1) {
      card.append(h("span", {
        class: "hint",
        text: actionConfirmationStage === "choose"
          ? 'Choose an exact match here, or say its full name or option number.'
          : `Selected: ${choices.find((choice) => choice.id === selectedAppChoiceId)?.label ?? "none"}. Confirm with Yes or No.`,
      }));
      const options = h("div", { class: "automation-actions app-choice-list" });
      for (const [index, choice] of choices.entries()) {
        const choose = h("button", {
          class: choice.id === selectedAppChoiceId ? "automation-approve" : "automation-deny",
          text: `Option ${index + 1}: ${choice.label}`,
          disabled: sending,
          onclick: () => selectAppChoice(choice),
        });
        options.append(choose);
      }
      card.append(options);
    }
    if (destination) {
      card.append(h("span", { class: "hint", text: "Destination" }));
      card.append(h("code", { class: "automation-path", text: destination }));
    }
    if (action.preview !== undefined && action.preview !== null) {
      card.append(h("span", { class: "hint", text: "Current contents (will be backed up)" }));
      card.append(h("pre", {
        class: "automation-content",
        text: action.preview || "(empty file)",
      }));
    }
    if (action.name === "write_file") {
      card.append(h("span", { class: "hint", text: "Replacement contents" }));
      card.append(h("pre", {
        class: "automation-content",
        text: content || "(empty file)",
      }));
    } else if (action.name === "create_file") {
      card.append(h("pre", { class: "automation-content", text: content || "(empty file)" }));
    }
    card.append(h("div", { class: "automation-actions" }, approve, deny));
    return card;
  }

  async function cancelRequest() {
    if (!activeRequestId || cancelRequested) return;
    cancelRequested = true;
    State.notify();
    send.disabled = true;
    send.title = "Cancelling…";
    try {
      await Bridge.chatCancel(activeRequestId);
    } catch (error) {
      console.error("[coucou] could not cancel chat request", error);
      cancelRequested = false;
      State.noteMessage = String(error).replace(/^Error:\s*/, "");
      State.view = "note";
      State.notify();
      send.disabled = false;
    }
  }

  send.addEventListener("click", () => {
    if (sending) void cancelRequest();
    else void submit();
  });
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      historyToggle.classList.toggle("selected", historyOpen);
      historyToggle.disabled = sending || !!pendingAction;
      newChat.disabled = sending || !!pendingAction;
      voiceButton.disabled = actionConfirmationActive
        || voiceState === "transcribing" || (sending && voiceState !== "speaking");
      if (sending) {
        send.textContent = cancelRequested ? "Cancelling…" : "Cancel";
        send.title = cancelRequested ? "Cancelling request" : "Cancel request";
        send.disabled = cancelRequested;
        send.classList.add("cancel");
      } else {
        send.disabled = false;
        send.title = "Send";
        send.classList.remove("cancel");
        if (!send.contains(sendIcon)) {
          clear(send);
          send.append(sendIcon);
        }
      }
      if (historyOpen) {
        const conversations = State.conversations;
        const historyKey = `${State.activeConversationId ?? ""}:${conversations
          .map((conversation) => `${conversation.id}:${conversation.updatedAt}:${conversation.title}`)
          .join("|")}`;
        if (historyList.dataset.key !== historyKey) {
          historyList.dataset.key = historyKey;
          clear(historyList);
          if (conversations.length === 0) {
            historyList.append(h("div", { class: "conversation-empty", text: "No past conversations yet." }));
          } else {
            for (const conversation of conversations) {
              const preview = [...conversation.messages].reverse().find((message) => message.role === "user")?.content
                ?? (conversation.file ? `File: ${conversation.file.name}` : "No messages yet");
              const select = h(
                "button",
                {
                  class: "conversation-item",
                  disabled: sending || !!pendingAction,
                  onclick: () => {
                    if (!State.selectConversation(conversation.id)) return;
                    historyOpen = false;
                    onHeightChange();
                  },
                },
                h("span", { class: "conversation-title", text: conversation.title }),
                h("span", { class: "conversation-preview", text: preview }),
                h("span", { class: "conversation-date", text: new Date(conversation.updatedAt).toLocaleString() }),
              );
              select.classList.toggle("active", conversation.id === State.activeConversationId);
              const remove = h("button", {
                class: "conversation-delete",
                text: "Delete",
                title: "Delete conversation",
                "aria-label": `Delete ${conversation.title}`,
                disabled: sending || !!pendingAction,
                onclick: () => {
                  if (!window.confirm(`Delete "${conversation.title}"? This cannot be undone.`)) return;
                  if (!State.deleteConversation(conversation.id)) return;
                  void Bridge.chatDelete(conversation.id).catch((err: unknown) => {
                    console.error("[coucou] could not clear deleted conversation from chat service", err);
                  });
                  onHeightChange();
                },
              });
              historyList.append(h("div", { class: "conversation-row" }, select, remove));
            }
          }
        }
      }
      historyList.hidden = !historyOpen;
      log.hidden = historyOpen;

      const thinking = State.stateOverride === "thinking";
      const key = `${State.activeConversationId ?? ""}:${State.chatHistory
        .map((message) => `${message.id}:${message.role}:${message.content}`)
        .join("|")}:${thinking}`;
      const actionKey = pendingAction && pendingConversationId === State.activeConversationId
        ? `${pendingAction.name}:${JSON.stringify(pendingAction.arguments)}:${actionConfirmationStage}:${selectedAppChoiceId ?? ""}`
        : "";
      if (`${key}:${actionKey}:${sending}` !== renderedKey) {
        renderedKey = `${key}:${actionKey}:${sending}`;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (pendingAction && pendingConversationId === State.activeConversationId) {
          log.append(actionCard(pendingAction));
        }
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending || !!pendingAction;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
