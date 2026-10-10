// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { ChatMessage, Settings, SharedConversationContext } from "./state";

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[coucou] ${cmd} failed`, err);
    return null;
  }
}

export interface BootInfo {
  settings: Settings;
  /** Logical screen rect of the monitor the island lives on. */
  screen: { x: number; y: number; width: number; height: number; scale: number };
  version: string;
  hookPath: string;
  /** False where the OS has no global cursor (Wayland): see Island.followPageCursor. */
  cursorPoll: boolean;
}

export interface OpenRouterAccount {
  id: string;
  name: string;
}

export interface MessageNotification {
  id: string;
  appName: string;
  title: string;
  body: string;
}

export interface SpotifyTrack {
  name: string;
  artists: string;
  imageUrl: string | null;
  durationMs: number;
  uri: string | null;
}

export interface SpotifySnapshot {
  connected: boolean;
  playing: boolean;
  progressMs: number;
  track: SpotifyTrack | null;
  queue: SpotifyTrack[];
}

export const Bridge = {
  boot: () => call<BootInfo>("boot"),

  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),
  saveSettingsStrict: (settings: Settings) => callOrThrow<void>("save_settings", { settings }),

  /** Shrink the window down to the invisible wake strip (hidden) or back to full. */
  setCollapsed: (collapsed: boolean) => call<void>("set_collapsed", { collapsed }),

  /**
   * Pushes the island shape in window coordinates. Rust flips click-through from
   * its own cursor poll, so the flag is never a frame behind a click.
   */
  setIslandRect: (x: number, y: number, width: number, height: number) =>
    call<void>("set_island_rect", { x, y, width, height }),

  /** Give the window keyboard focus (chat field) and take it away again. */
  focusWindow: (focused: boolean) => call<void>("focus_window", { focused }),

  reposition: () => call<void>("reposition"),

  openUrl: (url: string) => call<void>("open_url", { url }),
  openUrlStrict: (url: string) => callOrThrow<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),

  quit: () => call<void>("quit_app"),

  openSettingsWindow: () => call<void>("open_settings_window"),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Claude Code hooks ─────────────────────────────────────────────────────
  hooksStatus: () => call<HookStatus>("hooks_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean) => callOrThrow<HookPreview>("hooks_preview", { install }),
  /**
   * Writes ~/.claude/settings.json — only ever after an explicit click, and only
   * when the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("hooks_apply", { install, fingerprint }),

  approvalDecision: (requestId: string, decision: "allow" | "deny") =>
    call<void>("approval_decision", { requestId, decision }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
  /** "Nobody can act on this" — Claude Code asks in the terminal right away. */
  approvalDecline: (requestId: string) => call<void>("approval_decline", { requestId }),

  // ── Chat, files, secrets ──────────────────────────────────────────────────
  /** One chat turn. The API key and any file bytes never leave Rust. */
  chatSend: (
    conversationId: string,
    history: ChatMessage[],
    query: string,
    context: ChatContext | null,
    sharedContext: SharedConversationContext[],
    requestId: string,
    localOnly = false,
    voiceRequest = false,
  ) => callOrThrow<ChatReply>("chat_send", {
    conversationId,
    history,
    query,
    context,
    sharedContext,
    requestId,
    localOnly,
    voiceRequest,
  }),
  chatAction: (
    conversationId: string,
    approved: boolean,
    requestId: string,
    selectedAppId?: string,
  ) => callOrThrow<ChatReply>("chat_action", {
    conversationId,
    approved,
    requestId,
    selectedAppId,
  }),
  chatCancel: (requestId: string) =>
    callOrThrow<void>("chat_cancel", { requestId }),
  setVoiceActive: (active: boolean) =>
    callOrThrow<void>("set_voice_active", { active }),
  setWakeConversationActive: (active: boolean) =>
    callOrThrow<void>("set_wake_conversation_active", { active }),
  setWakeCalibration: async (active: boolean) => {
    if (!IS_TAURI) return;
    await emit("wake-calibration", active);
  },
  ttsSpeak: (text: string, rate: number, volume: number) =>
    callOrThrow<void>("tts_speak", { text, rate, volume }),
  ttsStop: () => callOrThrow<void>("tts_stop"),
  ttsIsSpeaking: () => callOrThrow<boolean>("tts_is_speaking"),
  ollamaStatus: (refresh = false) =>
    callOrThrow<OllamaStatus>("ollama_status", { refresh }),
  chatDelete: (conversationId: string) =>
    callOrThrow<void>("chat_delete", { conversationId }),
  chatReset: () => call<void>("chat_reset"),
  /** Copies a dropped file into the inbox. */
  ingestFile: (path: string) => callOrThrow<DroppedFile>("ingest_file", { path }),
  /** Opens the native file picker; selected files stay on the Rust side. */
  async pickFile(): Promise<string | null> {
    if (!IS_TAURI) return null;
    const selected = await open({ multiple: false, directory: false });
    return Array.isArray(selected) ? selected[0] ?? null : selected;
  },
  async pickAutomationFolder(): Promise<string | null> {
    if (!IS_TAURI) return null;
    const selected = await open({ multiple: false, directory: true });
    return Array.isArray(selected) ? selected[0] ?? null : selected;
  },
  /** Only ever tells you whether a key exists — never its value. */
  secretPresent: (key: string) => call<boolean>("secret_present", { key }),
  secretSet: (key: string, value: string) => callOrThrow<void>("secret_set", { key, value }),
  secretClear: (key: string) => callOrThrow<void>("secret_clear", { key }),
  openRouterAccounts: () => callOrThrow<OpenRouterAccount[]>("openrouter_accounts"),
  openRouterAccountAdd: (name: string, key: string) =>
    callOrThrow<OpenRouterAccount>("openrouter_account_add", { name, key }),
  openRouterAccountRemove: (id: string) =>
    callOrThrow<void>("openrouter_account_remove", { id }),
  openRouterAccountReveal: (id: string) =>
    callOrThrow<string>("openrouter_account_reveal", { id }),

  // ── Integrations ──────────────────────────────────────────────────────────
  refreshIntegration: (id: string) => call<void>("refresh_integration", { id }),
  requestMessageAccess: () => callOrThrow<string>("request_message_access"),
  messageAccessStatus: () => call<boolean>("message_access_status"),
  openMessageSource: (id: string) =>
    callOrThrow<void>("open_message_source", { notificationId: id }),
  spotifyConnect: () => callOrThrow<string>("spotify_connect"),
  spotifyControl: (
    action: "play" | "pause" | "next" | "previous" | "seek" | "play_track",
    positionMs?: number,
    trackUri?: string,
  ) => callOrThrow<void>("spotify_control", {
    action,
    positionMs: positionMs ?? null,
    trackUri: trackUri ?? null,
  }),
  spotifyDisconnect: () => callOrThrow<void>("spotify_disconnect"),

  /** Tray → Pause. Stops the integration pollers, not just the island. */
  setPaused: (paused: boolean) => call<void>("set_paused", { paused }),

  /** Tray → toggle the native auto-hide state. */
  toggleAutoHide: () => call<boolean>("toggle_auto_hide"),
};

export interface IntegrationUpdate {
  id: string;
  data: Record<string, unknown>;
  error: string | null;
  event: { success: boolean; label: string; detail: string | null } | null;
}

export type ChatContext =
  | { kind: "file"; name: string; path: string }
  | { kind: "window"; appName: string; title: string; url?: string };

export interface DroppedFile {
  name: string;
  path: string;
  size: number;
}

export interface ChatReply {
  text: string;
  model: string;
  action: AutomationAction | null;
  provider: string;
  fallbackNotice: string | null;
}

export interface OllamaStatus {
  reachable: boolean;
  models: string[];
  error: string | null;
}

export interface AutomationAction {
  name: string;
  arguments: Record<string, unknown>;
  preview?: string | null;
  previewHash?: number | null;
}

export interface HookStatus {
  installed: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
}

export interface HookPreview {
  diff: string;
  backup: string;
  settingsPath: string;
  /** Hand back to hooksApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

/** Same as `call`, but surfaces the error so the UI can show what went wrong. */
async function callOrThrow<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error("not running inside Coucou");
  return invoke<T>(cmd, args);
}

export type BridgeEvent =
  | { name: "cursor"; payload: { x: number; y: number } }
  | { name: "tray"; payload: string }
  | { name: "hook"; payload: Record<string, unknown> }
  | { name: "screen-changed"; payload: null };

export interface DragDropPayload {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
}

/** Files dragged onto the island. Only reaches us when the window takes the mouse. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  return getCurrentWebview().onDragDropEvent((event) => {
    handler(event.payload as DragDropPayload);
  });
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
