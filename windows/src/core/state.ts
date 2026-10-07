// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
}

export interface ApprovalInfo {
  requestId: string;
  sessionId: string;
  tool: string;
  command: string;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  model?: string;
  fallbackNotice?: string;
}

export interface ChatConversation {
  id: string;
  title: string;
  updatedAt: number;
  messages: ChatMessage[];
  file: { name: string; path: string } | null;
}

export interface SharedConversationContext {
  title: string;
  messages: ChatMessage[];
  file: { name: string; path: string } | null;
}

interface StoredConversations {
  activeId: string | null;
  conversations: ChatConversation[];
}

const CONVERSATION_STORAGE_KEY = "coucou.conversations.v1";

function readConversations(): StoredConversations {
  if (typeof window === "undefined") return { activeId: null, conversations: [] };
  try {
    const raw = window.localStorage.getItem(CONVERSATION_STORAGE_KEY);
    if (!raw) return { activeId: null, conversations: [] };
    const parsed: unknown = JSON.parse(raw);
    if (
      typeof parsed !== "object" || parsed === null ||
      !("conversations" in parsed) || !Array.isArray(parsed.conversations)
    ) {
      throw new Error("Conversation history has an invalid format.");
    }
    const conversations = parsed.conversations.filter((item): item is ChatConversation =>
      typeof item === "object" && item !== null &&
      "id" in item && typeof item.id === "string" &&
      "title" in item && typeof item.title === "string" &&
      "updatedAt" in item && typeof item.updatedAt === "number" &&
      "messages" in item && Array.isArray(item.messages) &&
      item.messages.every((message: unknown) =>
        typeof message === "object" && message !== null &&
        "id" in message && typeof message.id === "number" &&
        "role" in message && (message.role === "user" || message.role === "assistant") &&
        "content" in message && typeof message.content === "string",
      ) &&
      "file" in item &&
      (item.file === null ||
        (typeof item.file === "object" && item.file !== null &&
          "name" in item.file && typeof item.file.name === "string" &&
          "path" in item.file && typeof item.file.path === "string")),
    );
    const activeId =
      "activeId" in parsed && typeof parsed.activeId === "string" &&
      conversations.some((conversation) => conversation.id === parsed.activeId)
        ? parsed.activeId
        : null;
    return { activeId, conversations };
  } catch (error) {
    console.error("[coucou] could not restore conversation history", error);
    return { activeId: null, conversations: [] };
  }
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/** AgentTask.integrationAgents — same ids, names and colours as macOS. */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "VS Code", "#F5F6F8", "claudeCode"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  absenceInterval: number;
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  autoHide: boolean;
  /** OpenRouter model slug; openrouter/free enables per-request auto-routing. */
  model: string;
  automationFolders: string[];
  providerMode: "auto" | "ollamaOnly" | "openRouterOnly";
  ollamaModel: string;
  speakRepliesMode: "off" | "voiceOnly" | "always";
  ttsEngine: "auto" | "webSpeech" | "sapi";
  ttsVoice: string;
  ttsRate: number;
  ttsVolume: number;
  wakeWordEnabled: boolean;
  wakeWordPronunciation: string;
  wakeWordThreshold: number;
  /** Kept to read settings.json files written by earlier Windows builds. */
  spokenReplies: boolean;
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  autoHide: true,
  model: "openrouter/free",
  automationFolders: [],
  providerMode: "auto",
  ollamaModel: "qwen2.5:7b",
  speakRepliesMode: "voiceOnly",
  ttsEngine: "auto",
  ttsVoice: "",
  ttsRate: 1,
  ttsVolume: 1,
  wakeWordEnabled: true,
  wakeWordPronunciation: "Hey Macha",
  wakeWordThreshold: 0.012,
  spokenReplies: false,
};

type Listener = () => void;

class AppState {
  constructor() {
    const stored = readConversations();
    this.conversations = stored.conversations.sort((a, b) => b.updatedAt - a.updatedAt);
    this.activeConversationId = stored.activeId;
    const active = this.activeConversation;
    this.chatHistory = active?.messages.map((message) => ({ ...message })) ?? [];
    this.droppedFile = active?.file ? { ...active.file } : null;
    this.promptContext = active?.file
      ? { kind: "file", name: active.file.name, path: active.file.path }
      : null;
  }

  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  focusId: string | null = null;

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  conversations: ChatConversation[] = [];
  activeConversationId: string | null = null;
  pendingApproval: ApprovalInfo | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  private listeners = new Set<Listener>();

  get activeConversation(): ChatConversation | null {
    return this.conversations.find((conversation) => conversation.id === this.activeConversationId) ?? null;
  }

  startConversation(file: { name: string; path: string } | null = null): string {
    const id = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const conversation: ChatConversation = {
      id,
      title: file?.name ?? "New conversation",
      updatedAt: Date.now(),
      messages: [],
      file: file ? { ...file } : null,
    };
    this.conversations.unshift(conversation);
    this.activeConversationId = id;
    this.chatHistory = [];
    this.droppedFile = file ? { ...file } : null;
    this.promptContext = file ? { kind: "file", name: file.name, path: file.path } : null;
    this.persistConversations();
    this.notify();
    return id;
  }

  selectConversation(id: string): boolean {
    const conversation = this.conversations.find((item) => item.id === id);
    if (!conversation) return false;
    this.activeConversationId = id;
    this.chatHistory = conversation.messages.map((message) => ({ ...message }));
    this.droppedFile = conversation.file ? { ...conversation.file } : null;
    this.promptContext = conversation.file
      ? { kind: "file", name: conversation.file.name, path: conversation.file.path }
      : null;
    this.persistConversations();
    this.notify();
    return true;
  }

  deleteConversation(id: string): boolean {
    const index = this.conversations.findIndex((conversation) => conversation.id === id);
    if (index < 0) return false;
    this.conversations.splice(index, 1);
    if (this.activeConversationId === id) {
      const next = this.conversations[0] ?? null;
      this.activeConversationId = next?.id ?? null;
      this.chatHistory = next?.messages.map((message) => ({ ...message })) ?? [];
      this.droppedFile = next?.file ? { ...next.file } : null;
      this.promptContext = next?.file
        ? { kind: "file", name: next.file.name, path: next.file.path }
        : null;
    }
    this.persistConversations();
    this.notify();
    return true;
  }

  saveActiveConversation(): void {
    const conversation = this.activeConversation;
    if (!conversation) return;
    conversation.messages = this.chatHistory.map((message) => ({ ...message }));
    conversation.updatedAt = Date.now();
    const firstUserMessage = conversation.messages.find((message) => message.role === "user");
    if (firstUserMessage) {
      conversation.title = firstUserMessage.content.trim().replace(/\s+/g, " ").slice(0, 48) || "New conversation";
    }
    this.conversations.sort((a, b) => b.updatedAt - a.updatedAt);
    this.persistConversations();
  }

  updateActiveFile(file: { name: string; path: string }): void {
    const conversation = this.activeConversation;
    if (!conversation) return;
    conversation.file = { ...file };
    this.droppedFile = { ...file };
    this.promptContext = { kind: "file", name: file.name, path: file.path };
    this.persistConversations();
    this.notify();
  }

  sharedConversationContext(excludingId: string): SharedConversationContext[] {
    return this.conversations
      .filter((conversation) => conversation.id !== excludingId)
      .map((conversation) => ({
        title: conversation.title,
        messages: conversation.messages.map((message) => ({ ...message })),
        file: conversation.file ? { ...conversation.file } : null,
      }));
  }

  private persistConversations(): void {
    if (typeof window === "undefined") return;
    try {
      window.localStorage.setItem(
        CONVERSATION_STORAGE_KEY,
        JSON.stringify({ activeId: this.activeConversationId, conversations: this.conversations }),
      );
    } catch (error) {
      console.error("[coucou] could not save conversation history", error);
    }
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    t.pillBadge = null;
    this.notify();
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** loadIntegrationTasks() — VS Code always on, the rest opt-in (max 4). */
  loadIntegrationTasks() {
    for (const proto of INTEGRATION_AGENTS) {
      const shouldLoad =
        proto.id === "integration_claude" || this.settings.activeIntegrations.includes(proto.id);
      const idx = this.tasks.findIndex((t) => t.id === proto.id);
      if (shouldLoad && idx < 0) this.tasks.push({ ...proto, steps: [] });
      if (!shouldLoad && idx >= 0) this.tasks.splice(idx, 1);
    }
    // Order: integration_claude first, then agent_* pills (visible in slice(0,4)),
    // then other integrations in declaration order.
    const order = INTEGRATION_AGENTS.map((t) => t.id);
    this.tasks.sort((a, b) => {
      const isAgentA = a.id.startsWith("agent_");
      const isAgentB = b.id.startsWith("agent_");
      // integration_claude always first
      if (a.id === "integration_claude") return -1;
      if (b.id === "integration_claude") return 1;
      // agent_* before other integrations; preserve insertion order among themselves
      if (isAgentA && !isAgentB) return -1;
      if (isAgentB && !isAgentA) return 1;
      if (isAgentA && isAgentB) return 0;
      // both known integrations → declaration order
      return order.indexOf(a.id) - order.indexOf(b.id);
    });
    if (!this.focusId) this.focusId = "integration_claude";
    this.notify();
  }

  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    this.tasks.splice(idx, 1);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? "integration_claude";
    this.notify();
  }

  /** Creates a dynamic agent_ pill on first event; no-ops if it already exists.
   *  Inserted right after integration_claude so it appears in the visible slice(0,4). */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    const at = this.tasks.findIndex((t) => t.id === "integration_claude") + 1;
    this.tasks.splice(at, 0, {
      id, name, color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
    if (!this.focusId) this.focusId = id;
    this.notify();
  }

  toggleIntegration(id: string) {
    if (id === "integration_claude") return;
    const active = this.settings.activeIntegrations;
    if (active.includes(id)) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = "integration_claude";
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.loadIntegrationTasks();
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
