// Integration cards shown in the overview's left card — DOM ports of
// IntegrationCardView and friends from IslandViewContent.swift.
//
// Cal.com is the one simplification: macOS shows a three-level calendar
// (month → day → booking); here it is the list of upcoming bookings.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { State, type AgentTask } from "../core/state";
import {
  Bridge,
  type MessageNotification,
  type SpotifySnapshot,
  type SpotifyTrack,
} from "../core/bridge";

/** Same shape as the Swift `timeAgo` computed properties. */
export function timeAgo(value: unknown): string {
  const date = typeof value === "number" ? new Date(value) : new Date(String(value));
  const diff = (Date.now() - date.getTime()) / 1000;
  if (!Number.isFinite(diff)) return "";
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function header(color: string, name: string, kind: string, extra?: Node): HTMLElement {
  const row = h("div", { class: "int-head" }, dot(color, 7), h("b", { text: name }), h("span", { text: kind }));
  if (extra) row.append(extra);
  return row;
}

/** Highlighted first row + plain rows, the layout every list card shares. */
function listRow(accent: string, first: boolean, ...children: Node[]): HTMLElement {
  const row = h("div", { class: first ? "int-row first" : "int-row" }, dot(accent, 5), ...children);
  if (first) row.style.background = `${accent}14`;
  return row;
}

function get(id: string): Record<string, unknown> {
  return (State.integrations[id]?.data ?? {}) as Record<string, unknown>;
}

function arr(id: string, key: string): Record<string, unknown>[] {
  const v = get(id)[key];
  return Array.isArray(v) ? (v as Record<string, unknown>[]) : [];
}

// ── Not configured / idle ─────────────────────────────────────────────────────

const OPEN_URLS: Record<string, string> = {
  integration_vercel: "https://vercel.com/dashboard",
  integration_github: "https://github.com",
  integration_stripe: "https://dashboard.stripe.com/payments",
  integration_notion: "https://notion.so",
  integration_calcom: "https://app.cal.com/bookings",
};

function idleCard(task: AgentTask, openSettings: () => void): HTMLElement {
  const info = State.integrations[task.id];
  const configured = info?.configured ?? false;
  const error = info?.error ?? null;
  // The Claude Code pill is about hooks, not a key — the macOS wording would be
  // misleading here.
  const missing = task.id === "integration_claude" ? "Hooks not installed" : "Key not configured";
  const label = error ?? (configured ? "Connected · loading…" : missing);
  const statusColor = error || !configured ? "#F4505E" : "#22C55E";

  const actions = h("div", { class: "int-actions" });
  if (task.id === "integration_claude") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}b3`,
        text: "Open Visual Studio Code",
        onclick: () => void Bridge.openInVSCode(task.sessionCwd ?? null),
      }),
    );
  } else if (OPEN_URLS[task.id]) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: `Open ${task.name}`,
        onclick: () => void Bridge.openUrl(OPEN_URLS[task.id]),
      }),
    );
  }
  if (configured) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Refresh",
        onclick: () => void Bridge.refreshIntegration(task.id),
      }),
    );
  } else {
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Settings…", onclick: openSettings }),
    );
  }

  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.id === "integration_claude" ? "VS Code" : task.name, "Integration"),
    h("div", { class: "int-status" }, dot(statusColor, 5), h("span", { text: label })),
    actions,
  );
}

// ── Vercel ────────────────────────────────────────────────────────────────────

function vercelCard(onDetail: () => void): HTMLElement {
  const deployments = arr("integration_vercel", "deployments");
  const rows = h("div", { class: "int-rows" });
  deployments.slice(0, 3).forEach((d, i) => {
    const accent = d.state === "READY" ? "#22C55E" : "#F4505E";
    const name = h("span", { class: "int-name", text: String(d.projectName ?? "") });
    const ago = h("span", { class: "int-ago", text: timeAgo(d.createdAt) });
    if (i === 0) {
      const more = h(
        "button",
        { class: "int-more", title: "Details", onclick: onDetail },
        svg(ICONS.ellipsis, 8),
      );
      rows.append(listRow(accent, true, name, ago, more));
    } else {
      rows.append(listRow(accent, false, name, ago));
    }
  });
  return h("div", { class: "int-card" }, header("#7C5CFF", "Vercel", "Deployments"), rows);
}

function vercelDetail(onBack: () => void): HTMLElement {
  const d = arr("integration_vercel", "deployments")[0] ?? {};
  const success = d.state === "READY";
  const accent = success ? "#22C55E" : "#F4505E";
  const status = success ? "Ready" : d.state === "CANCELED" ? "Canceled" : "Error";
  const body = h("div", { class: "int-detail-body" });
  if (d.commitMessage) body.append(h("div", { class: "int-commit", text: String(d.commitMessage) }));
  const meta = h("div", { class: "int-meta" });
  if (d.branch) meta.append(h("span", { text: String(d.branch) }));
  meta.append(h("span", { text: `${timeAgo(d.createdAt)} ago` }));
  body.append(meta);
  if (d.url) {
    body.append(
      h("button", {
        class: "int-link",
        text: String(d.url),
        onclick: () => void Bridge.openUrl(`https://${d.url}`),
      }),
    );
  }
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: String(d.projectName ?? "Deployment") }),
      h("span", { class: "int-badge", style: `color:${accent};background:${accent}24`, text: status }),
    ),
    body,
  );
}

// ── GitHub ────────────────────────────────────────────────────────────────────

function statRow(icon: string, color: string, label: string, value: string): HTMLElement {
  return h(
    "div",
    { class: "int-stat" },
    h("i", { class: "int-stat-icon", style: `color:${color}` }, svg(icon, 10)),
    h("span", { class: "int-stat-label", text: label }),
    h("span", { class: "int-stat-value", text: value }),
  );
}

function githubCard(): HTMLElement {
  const d = get("integration_github");
  const stars = Number(d.totalStars ?? 0);
  const repos = Number(d.totalRepos ?? 0);
  const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  return h(
    "div",
    { class: "int-card" },
    header("#F4505E", "GitHub", "Overview"),
    h(
      "div",
      { class: "int-stats" },
      statRow(ICONS.star, "#F5A524", "Total stars", fmt(stars)),
      statRow(ICONS.stack, "#6B7079", "Repositories", String(repos)),
    ),
  );
}

// ── Stripe ────────────────────────────────────────────────────────────────────

function stripeCard(): HTMLElement {
  const d = get("integration_stripe");
  const balance = (Number(d.balance ?? 0) / 100).toFixed(2);
  const currency = String(d.currency ?? "eur").toUpperCase();
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_stripe", "payments")) {
    const success = p.status === "succeeded";
    const accent = success ? "#22C55E" : "#F4505E";
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot(accent, 5),
        h("span", { class: "int-name", text: String(p.description ?? "Payment") }),
        h("span", {
          class: "int-amount",
          style: "color:#22c55e",
          text: `+${(Number(p.amount ?? 0) / 100).toFixed(2)}`,
        }),
        h("span", { class: "int-ago", text: timeAgo(p.createdAt) }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card" },
    header("#0570DE", "Stripe", "Payments"),
    h("div", { class: "int-balance" }, h("span", { text: balance }), h("i", { text: currency })),
    rows,
  );
}

// ── Notion ────────────────────────────────────────────────────────────────────

function notionCard(): HTMLElement {
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_notion", "pages").slice(0, 3)) {
    rows.append(
      h(
        "button",
        {
          class: "int-page",
          onclick: () => {
            if (typeof p.url === "string") void Bridge.openUrl(p.url);
          },
        },
        p.emoji
          ? h("span", { class: "int-emoji", text: String(p.emoji) })
          : h("i", { class: "int-emoji" }, svg(ICONS.doc, 9)),
        h("span", { class: "int-name", text: String(p.title ?? "Untitled") }),
        h("span", { class: "int-ago", text: timeAgo(p.lastEditedAt) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#E8E8E8", "Notion", "Recent"), rows);
}

// ── Cal.com ───────────────────────────────────────────────────────────────────

function calcomCard(): HTMLElement {
  const bookings = arr("integration_calcom", "bookings")
    .slice()
    .sort((a, b) => new Date(String(a.start)).getTime() - new Date(String(b.start)).getTime());
  const rows = h("div", { class: "int-rows tight" });
  if (bookings.length === 0) {
    rows.append(h("div", { class: "int-empty", text: "No calls scheduled" }));
  }
  for (const b of bookings.slice(0, 3)) {
    const when = new Date(String(b.start));
    const day = when.toLocaleDateString(undefined, { day: "2-digit", month: "2-digit" });
    const time = when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot("#C9956A", 4),
        h("span", { class: "int-time", text: `${day} ${time}` }),
        h("span", { class: "int-name", text: String(b.title ?? "Meeting") }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#C9956A", "Cal.com", "Schedule"), rows);
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

export interface IntegrationCardHooks {
  detailOpen: boolean;
  openDetail(): void;
  closeDetail(): void;
  openSettings(): void;
}

/** True when this integration has data worth showing instead of the idle card. */
export function hasIntegrationData(id: string): boolean {
  const info = State.integrations[id];
  if (!info || info.error) return false;
  switch (id) {
    case "integration_vercel":
      return arr(id, "deployments").length > 0;
    case "integration_messages":
    case "integration_spotify":
      return info.loaded;
    case "integration_github":
      return get(id).totalRepos != null;
    case "integration_stripe":
      return info.loaded;
    case "integration_notion":
      return arr(id, "pages").length > 0;
    case "integration_calcom":
      return info.loaded;
    default:
      return false;
  }
}

function messageNotification(): MessageNotification | null {
  const data = get("integration_messages");
  const value = Array.isArray(data.notifications) ? data.notifications[0] : data.notification;
  if (typeof value !== "object" || value === null) return null;
  if (!("id" in value) || typeof value.id !== "string"
    || !("appName" in value) || typeof value.appName !== "string"
    || !("title" in value) || typeof value.title !== "string"
    || !("body" in value) || typeof value.body !== "string") return null;
  return { id: value.id, appName: value.appName, title: value.title, body: value.body };
}

function spotifyTrack(value: unknown): SpotifyTrack | null {
  if (typeof value !== "object" || value === null
    || !("name" in value) || typeof value.name !== "string"
    || !("artists" in value) || typeof value.artists !== "string"
    || !("durationMs" in value) || typeof value.durationMs !== "number") return null;
  return {
    name: value.name,
    artists: value.artists,
    durationMs: value.durationMs,
    imageUrl: "imageUrl" in value && typeof value.imageUrl === "string" ? value.imageUrl : null,
    uri: "uri" in value && typeof value.uri === "string" ? value.uri : null,
  };
}

function spotifySnapshot(): SpotifySnapshot {
  const data = get("integration_spotify");
  return {
    connected: data.connected === true,
    playing: data.playing === true,
    progressMs: typeof data.progressMs === "number" ? data.progressMs : 0,
    track: spotifyTrack(data.track),
    queue: Array.isArray(data.queue)
      ? data.queue.map(spotifyTrack).filter((track): track is SpotifyTrack => track !== null)
      : [],
  };
}

async function spotifyAction(
  action: "play" | "pause" | "next" | "previous" | "seek" | "play_track",
  positionMs?: number,
  trackUri?: string,
): Promise<void> {
  try {
    await Bridge.spotifyControl(action, positionMs, trackUri);
    const info = State.integrations.integration_spotify;
    if (info) info.error = null;
    void Bridge.refreshIntegration("integration_spotify");
  } catch (error) {
    const info = State.integrations.integration_spotify;
    if (info) info.error = String(error).replace(/^Error:\s*/, "");
    console.error("[coucou] Spotify playback action failed", error);
  }
  State.notify();
}

function messagesCard(openSettings: () => void): HTMLElement {
  const message = messageNotification();
  if (!message) {
    return h(
      "div",
      { class: "int-card" },
      header("#22C55E", "Messages", "Windows notifications"),
      h("div", {
        class: "int-status",
        text: State.integrations.integration_messages?.error
          ?? (State.integrations.integration_messages?.configured
            ? "Waiting for notifications…"
            : "Allow notification access in Settings."),
      }),
      h("div", { class: "int-actions" },
        h("button", {
          class: "link-btn",
          text: "Notification settings",
          onclick: openSettings,
        }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card message-notification" },
    header("#22C55E", "Messages", message.appName),
    State.integrations.integration_messages?.error
      ? h("div", { class: "int-status", text: State.integrations.integration_messages.error })
      : null,
    h("button", {
      class: "message-notification-content",
      title: "Open source app",
      onclick: async () => {
        try {
          await Bridge.openMessageSource(message.id);
        } catch (error) {
          const info = State.integrations.integration_messages;
          if (info) info.error = String(error).replace(/^Error:\s*/, "");
          console.error("[coucou] could not open notification source", error);
          State.notify();
        }
      },
    },
    h("b", { text: message.title || message.appName }),
    h("span", { text: message.body }),
    ),
  );
}

function formatPlaybackTime(milliseconds: number): string {
  const seconds = Math.floor(Math.max(0, milliseconds) / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function spotifyIcon(path: string, size = 16): SVGSVGElement {
  return svg(path, size, { stroke: 1.9 });
}

const SPOTIFY_ICONS = {
  previous: "M6 5v14M18 6v12L8 12l10-6z",
  play: "M8 5v14l11-7z",
  pause: "M7 5h4v14H7zM15 5h4v14h-4z",
  next: "M18 5v14M6 6v12l10-6L6 6z",
  rowPlay: "M8 5v14l11-7z",
};

function spotifyCard(openSettings: () => void): HTMLElement {
  const data = spotifySnapshot();
  const error = State.integrations.integration_spotify?.error;
  const track = data.track;
  if (!track) {
    return h(
      "div",
      { class: "int-card spotify-card" },
      header("#1DB954", "Spotify", "Player"),
      h("div", {
        class: "int-status",
        text: error ?? (data.connected
          ? "Nothing is playing."
          : "Connect Spotify to see playback."),
      }),
      h("div", { class: "int-actions" },
        h("button", {
          class: "link-btn",
          text: data.connected ? "Refresh" : "Connect Spotify",
          onclick: () => data.connected
            ? void Bridge.refreshIntegration("integration_spotify")
            : openSettings(),
        }),
      ),
    );
  }

  const seek = h("input", {
    class: "spotify-seek",
    type: "range",
    min: "0",
    max: String(track.durationMs),
    value: String(Math.min(data.progressMs, track.durationMs)),
    "aria-label": "Track position",
  }) as HTMLInputElement;
  const updateSeekProgress = () => {
    const progress = track.durationMs > 0
      ? Math.min(100, (Number(seek.value) / track.durationMs) * 100)
      : 0;
    seek.style.setProperty("--spotify-progress", `${progress}%`);
  };
  updateSeekProgress();
  seek.addEventListener("input", updateSeekProgress);
  seek.addEventListener("change", () => {
    void spotifyAction("seek", Number(seek.value));
  });
  const controls = h("div", { class: "spotify-controls" },
    h("button", {
      class: "spotify-skip",
      type: "button",
      title: "Previous track",
      "aria-label": "Previous track",
      onclick: () => void spotifyAction("previous"),
    }, spotifyIcon(SPOTIFY_ICONS.previous)),
    h("button", {
      class: "spotify-toggle",
      type: "button",
      title: data.playing ? "Pause" : "Play",
      "aria-label": data.playing ? "Pause" : "Play",
      onclick: () => void spotifyAction(data.playing ? "pause" : "play"),
    }, spotifyIcon(data.playing ? SPOTIFY_ICONS.pause : SPOTIFY_ICONS.play, 21)),
    h("button", {
      class: "spotify-skip",
      type: "button",
      title: "Next track",
      "aria-label": "Next track",
      onclick: () => void spotifyAction("next"),
    }, spotifyIcon(SPOTIFY_ICONS.next)),
  );

  const queueRows = h("div", { class: "spotify-queue" });
  const queueHeader = h("div", { class: "spotify-queue-header" },
    h("b", { text: "Up next" }),
    h("span", { text: `${data.queue.length} ${data.queue.length === 1 ? "track" : "tracks"}` }),
  );
  const queueList = h("div", { class: "spotify-queue-list" });
  if (data.queue.length) {
    data.queue.forEach((item, index) => {
      const thumbnail = item.imageUrl
        ? h("img", { class: "spotify-queue-artwork", src: item.imageUrl, alt: "" })
        : h("span", { class: "spotify-queue-artwork spotify-queue-artwork-empty", "aria-hidden": "true" });
      const numberBadge = h("span", { class: "spotify-queue-number", "aria-hidden": "true" },
        h("span", { class: "spotify-queue-number-value", text: String(index + 1) }),
        spotifyIcon(SPOTIFY_ICONS.rowPlay, 12),
      );
      const row = h("button", {
        class: `spotify-queue-item${index === 0 ? " is-next" : ""}`,
        type: "button",
        title: item.uri ? `Play ${item.name}` : `${item.name} cannot be selected from this queue`,
        "aria-label": `Play ${item.name} by ${item.artists}`,
        disabled: !item.uri,
        onclick: () => {
          if (item.uri) void spotifyAction("play_track", undefined, item.uri);
        },
      },
      thumbnail,
      h("span", { class: "spotify-queue-copy" },
        h("b", { text: item.name }),
        h("small", { text: item.artists }),
      ),
      index === 0
        ? h("span", { class: "spotify-equalizer", "aria-label": "Next track" },
          h("i"), h("i"), h("i"),
        )
        : numberBadge,
      h("small", { class: "spotify-queue-duration", text: formatPlaybackTime(item.durationMs) }),
      );
      row.style.setProperty("--spotify-row-index", String(index));
      queueList.append(row);
    });
  } else {
    queueList.append(h("small", { class: "spotify-queue-empty", text: "Queue is empty" }));
  }
  queueRows.append(queueHeader, queueList);

  const artwork = track.imageUrl
    ? h("img", {
      class: `spotify-artwork${data.playing ? " is-playing" : ""}`,
      src: track.imageUrl,
      alt: "Album artwork",
    })
    : h("div", {
      class: `spotify-artwork spotify-artwork-empty${data.playing ? " is-playing" : ""}`,
      text: "♪",
    });
  const nowPlaying = h("div", { class: "spotify-now-playing" },
    artwork,
    h("div", { class: "spotify-track" },
      h("b", { text: track.name }),
      h("span", { text: track.artists }),
    ),
  );
  const timeline = h("div", { class: "spotify-timeline" },
    h("span", { text: formatPlaybackTime(data.progressMs) }),
    seek,
    h("span", { text: formatPlaybackTime(track.durationMs) }),
  );
  return h(
    "div",
    { class: "int-card spotify-card" },
    header("#1DB954", "Spotify", "Now playing"),
    error ? h("div", { class: "int-status", text: error }) : null,
    nowPlaying,
    timeline,
    controls,
    queueRows,
  );
}

export function renderIntegrationCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  if (task.id === "integration_messages") return messagesCard(hooks.openSettings);
  if (task.id === "integration_spotify") return spotifyCard(hooks.openSettings);
  if (task.id === "integration_vercel" && hasIntegrationData(task.id)) {
    return hooks.detailOpen ? vercelDetail(hooks.closeDetail) : vercelCard(hooks.openDetail);
  }
  if (!hasIntegrationData(task.id)) return idleCard(task, hooks.openSettings);

  switch (task.id) {
    case "integration_github":
      return githubCard();
    case "integration_stripe":
      return stripeCard();
    case "integration_notion":
      return notionCard();
    case "integration_calcom":
      return calcomCard();
    default:
      return idleCard(task, hooks.openSettings);
  }
}

export { clear };
