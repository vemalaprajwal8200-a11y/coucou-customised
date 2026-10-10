// Integration events → island state. Port of the `handle…` methods in the Swift
// pollers: a genuinely new item flips the pill to finished/error, badges it when
// the pill isn't focused, plays a sound, and clears itself after 60 s.

import {
  onEvent,
  Bridge,
  type IntegrationUpdate,
  type MessageNotification,
  type SpotifySnapshot,
} from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { Island } from "./island";

/** Which Credential Manager key backs each pill. */
const KEY_FOR: Record<string, string> = {
  integration_stripe: "stripe-api-key",
  integration_github: "github-token",
  integration_vercel: "vercel-token",
  integration_spotify: "spotify-client-id",
  integration_notion: "notion-api-key",
  integration_calcom: "calcom-api-key",
};

const clearTimers = new Map<string, number>();

export function registerIntegrationHandlers(island: Island) {
  void onEvent<IntegrationUpdate>("integration", (update) => handle(island, update));
  void onEvent<MessageNotification>("message-notification", (notification) => {
    if (State.paused) return;
    const previous = State.integrations.integration_messages;
    const queued = Array.isArray(previous?.data.notifications)
      ? previous.data.notifications.filter((item): item is MessageNotification =>
        typeof item === "object" && item !== null
        && "id" in item && typeof item.id === "string"
        && "appName" in item && typeof item.appName === "string"
        && "title" in item && typeof item.title === "string"
        && "body" in item && typeof item.body === "string")
      : [];
    if (!queued.some((item) => item.id === notification.id)) queued.push(notification);
    State.integrations.integration_messages = {
      data: { ...(previous?.data ?? {}), notification: queued[0], notifications: queued },
      error: null,
      loaded: true,
      configured: true,
    };
    island.showMessageNotification();
  });
  void onEvent<string>("message-access-error", (message) => {
    const previous = State.integrations.integration_messages;
    State.integrations.integration_messages = {
      ...(previous ?? { data: {}, loaded: false, configured: false }),
      error: message,
    };
    State.notify();
  });
  void onEvent<SpotifySnapshot>("spotify-update", (snapshot) => {
    const previous = State.integrations.integration_spotify;
    State.integrations.integration_spotify = {
      data: { ...snapshot },
      error: null,
      loaded: true,
      configured: previous?.configured ?? true,
    };
    State.notify();
  });
  void onEvent<string>("spotify-error", (message) => {
    const previous = State.integrations.integration_spotify;
    State.integrations.integration_spotify = {
      ...(previous ?? { data: {}, loaded: false, configured: false }),
      error: message,
    };
    State.notify();
  });
  void refreshConfigured();
}

/** Asks Rust which keys exist so the idle cards can say so. */
export async function refreshConfigured() {
  for (const [id, key] of Object.entries(KEY_FOR)) {
    const present = (await Bridge.secretPresent(key)) ?? false;
    const info = State.integrations[id] ?? { data: {}, error: null, loaded: false, configured: false };
    State.integrations[id] = { ...info, configured: present };
  }
  const notificationAccess = await Bridge.messageAccessStatus();
  const messages = State.integrations.integration_messages;
  State.integrations.integration_messages = {
    data: messages?.data ?? {},
    error: null,
    loaded: notificationAccess ?? false,
    configured: notificationAccess ?? false,
  };
  const hooks = State.settings.hooksInstalled;
  const claude = State.integrations.integration_claude ?? {
    data: {}, error: null, loaded: false, configured: false,
  };
  State.integrations.integration_claude = { ...claude, configured: hooks };
  State.notify();
}

function handle(island: Island, update: IntegrationUpdate) {
  if (State.paused) return;

  const previous = State.integrations[update.id];
  State.integrations[update.id] = {
    data: update.error ? (previous?.data ?? {}) : update.data,
    error: update.error,
    loaded: update.error ? (previous?.loaded ?? false) : true,
    configured: previous?.configured ?? true,
  };

  const event = update.event;
  if (event) {
    const task = State.tasks.find((t) => t.id === update.id);
    if (task) {
      task.state = event.success ? "finished" : "error";
      task.steps = event.detail ? [event.label, event.detail] : [event.label];
      task.stepIndex = task.steps.length - 1;
      if (State.focusId !== update.id) {
        task.pillBadge = event.success ? "finished" : "error";
      }
      Sound.play(event.success ? "finish" : "error");
      // Same as the Swift pollers: show the compact island so the badge is seen,
      // but never steal the screen for a successful deploy.
      island.reveal();

      const existing = clearTimers.get(update.id);
      if (existing != null) window.clearTimeout(existing);
      clearTimers.set(
        update.id,
        window.setTimeout(() => {
          clearTimers.delete(update.id);
          const t = State.tasks.find((x) => x.id === update.id);
          if (!t || (t.state !== "finished" && t.state !== "error")) return;
          t.state = "idle";
          t.steps = [];
          t.stepIndex = 0;
          t.pillBadge = null;
          State.notify();
        }, 60_000),
      );
    }
  }

  State.notify();
}
