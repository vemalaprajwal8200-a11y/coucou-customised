// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type ChatContext } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";

let nextId = 1;

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
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

export function buildPrompt(onHeightChange: () => void): ViewHost {
  let sending = false;
  let historyOpen = false;
  let renderedKey = "";

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
      if (sending) return;
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
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);

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

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    const conversationId = State.activeConversationId ?? State.startConversation();
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
      const reply = await Bridge.chatSend(conversationId, history, query, context, sharedContext);
      State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
      State.saveActiveConversation();
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      const lastMessage = State.chatHistory[State.chatHistory.length - 1];
      if (lastMessage?.role === "user" && lastMessage.content === query) {
        State.chatHistory.pop();
        State.saveActiveConversation();
      }
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => void submit());
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
      historyToggle.disabled = sending;
      newChat.disabled = sending;
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
                  disabled: sending,
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
                disabled: sending,
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
      if (key !== renderedKey) {
        renderedKey = key;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
