// Settings window — the place where anything that writes to disk is confirmed.
// Stage 2 covers the Claude Code hooks and the general preferences; API keys and
// integrations land here too in a later stage.

import "./settings.css";
import {
  Bridge,
  onEvent,
  type HookStatus,
  type OllamaStatus,
  type OpenRouterAccount,
} from "../core/bridge";
import { DEFAULT_SETTINGS, type Settings } from "../core/state";
import { State } from "../core/state";
import * as TTS from "../core/tts";
import { recordWakePronunciation } from "../core/wakeWord";
import { h, clear } from "../views/dom";

let settings: Settings = { ...DEFAULT_SETTINGS };
let version = "";

const root = document.getElementById("settings-root")!;

async function save() {
  State.settings = { ...State.settings, ...settings };
  await Bridge.saveSettings(settings);
}

// ── Reusable bits ─────────────────────────────────────────────────────────────

function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

function statusDot(ok: boolean): HTMLElement {
  return h("i", { class: "dot", style: `background:${ok ? "#22c55e" : "#f4505e"}` });
}

function renderDiff(text: string): HTMLElement {
  const box = h("div", { class: "diff" });
  for (const line of text.split("\n")) {
    const cls = line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "ctx";
    box.append(h("div", { class: cls, text: line }));
  }
  return box;
}

// ── Claude Code section ───────────────────────────────────────────────────────

function claudeSection(status: HookStatus): HTMLElement {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const section = h(
    "section",
    {},
    h("h2", {}, statusDot(status.installed), h("span", { text: "Claude Code" })),
    body,
  );

  const rebuild = async () => {
    const fresh = await Bridge.hooksStatus();
    if (fresh) Object.assign(status, fresh);
    clear(body);
    draw();
    const head = section.querySelector("h2")!;
    clear(head);
    head.append(statusDot(status.installed), h("span", { text: "Claude Code" }));
  };

  function draw() {
    body.append(
      h("div", {
        class: "hint",
        text: status.installed
          ? "Coucou is hooked into your Claude Code sessions. Tool calls, questions and permission requests show up in the island, and you can answer them there."
          : "Install the hooks to see your Claude Code sessions in the island and approve permissions without leaving what you are doing.",
      }),
      h("div", { class: "row" },
        h("label", { text: "settings.json" }),
        h("span", { class: "path", text: status.settingsPath }),
      ),
      h("div", { class: "row" },
        h("label", { text: "Relay" }),
        h("span", { class: "path", text: status.hookPath }),
        statusDot(status.hookReady),
      ),
    );

    if (!status.hookReady) {
      body.append(h("div", {
        class: "notice warn",
        text: "coucou-hook.exe is not in place yet. Restart Coucou; if it still fails, build it with `cargo build -p coucou-hook`.",
      }));
    }

    const actions = h("div", { class: "row" });
    const install = h("button", {
      class: "primary",
      text: status.installed ? "Reinstall hooks…" : "Install hooks…",
      onclick: () => showPreview(true),
    });
    // Writing hook commands that point at a relay which isn't there would give
    // every Claude Code session a broken hook and nothing to show for it.
    if (!status.hookReady) {
      install.disabled = true;
      install.title = "The relay isn't installed yet.";
    }
    actions.append(install);
    if (status.installed) {
      actions.append(h("button", {
        class: "danger",
        text: "Uninstall hooks…",
        onclick: () => showPreview(false),
      }));
    }
    body.append(actions);
  }

  async function showPreview(install: boolean) {
    let preview;
    try {
      preview = await Bridge.hooksPreview(install);
    } catch (err) {
      // An unreadable or invalid settings.json stops here rather than being
      // treated as empty and written over.
      clear(body);
      body.append(
        h("div", { class: "notice err", text: String(err).replace(/^Error:\s*/, "") }),
        h("div", { class: "row" }, h("button", {
          text: "Back",
          onclick: () => { clear(body); draw(); },
        })),
      );
      return;
    }
    if (!preview) return;
    clear(body);
    body.append(
      h("div", {
        class: "hint",
        text: install
          ? "This is exactly what will change in your settings.json. Your own hooks are left untouched."
          : "This removes Coucou's entries only. Your own hooks are left untouched.",
      }),
      renderDiff(preview.diff),
      h("div", { class: "row" },
        h("span", { class: "path", text: `Backup → ${preview.backup}` }),
      ),
    );
    const confirm = h("button", {
      class: install ? "primary" : "danger",
      text: install ? "Back up and write" : "Back up and remove",
    });
    confirm.addEventListener("click", async () => {
      confirm.disabled = true;
      try {
        const backup = await Bridge.hooksApply(install, preview.fingerprint);
        clear(body);
        body.append(h("div", {
          class: "notice ok",
          text: `Done. Previous settings saved as ${backup}. Open a new Claude Code session to pick the hooks up.`,
        }));
        window.setTimeout(() => void rebuild(), 2600);
      } catch (err) {
        confirm.disabled = false;
        body.append(h("div", { class: "notice err", text: `Could not write: ${String(err)}` }));
      }
    });
    body.append(h("div", { class: "row" }, confirm, h("button", {
      text: "Cancel",
      onclick: () => { clear(body); draw(); },
    })));
  }

  draw();
  return section;
}

// ── Claude API section ────────────────────────────────────────────────────────

const MODELS: [string, string][] = [
  ["openrouter/free", "Automatic — select a free model for each task"],
  ["stealth/space-bunny-alpha", "Space Bunny Alpha — general, coding, reasoning, vision"],
  ["nvidia/nemotron-3-ultra-550b-a5b5:free", "Nemotron 3 Ultra 550B — complex reasoning, planning, coding"],
  ["poolside/laguna-s-2.1:free", "Laguna S 2.1 — software engineering and coding agents"],
  ["nvidia/nemotron-3.5-lightning:free", "Nemotron 3.5 Lightning — fast agentic tasks"],
  ["dots-studio/dots-3-note-preview:free", "Dots 3 Note — reasoning, coding, multimodal, long context"],
  ["inclusionai/ling-3.0-flash-sante:free", "Ling 3.0 Flash Sante — health and evidence-based reasoning"],
  ["nvidia/nemotron-3-super-120b-a12b:free", "Nemotron 3 Super — general reasoning and planning"],
  ["thinkingmachines/inkling:free", "Inkling — reasoning, coding, tools, multilingual, vision/audio"],
  ["thinkingmachines/inkling-small:free", "Inkling Small — fast reasoning, coding, agents, multilingual"],
  ["qwen/qwen3.8-27b:free", "Qwen 3.8 27B — coding, research, agents, vision"],
  ["cohere/north-mini-code:free", "Cohere North Mini Code — agentic software engineering"],
  ["poolside/laguna-xs-2.1:free", "Laguna XS 2.1 — fast coding agent"],
  ["apodex/apodex-1.1-mini:free", "Apodex 1.1 Mini — research, forecasting, files and code"],
];

function apiSection(
  initialAccounts: OpenRouterAccount[],
  initialOllamaStatus: OllamaStatus,
  accountLoadError?: string,
): HTMLElement {
  let accounts = initialAccounts;
  let ollama = initialOllamaStatus;
  const dot = statusDot(accounts.length > 0 || ollama.reachable);
  const state = h("span", { class: "hint" });
  const feedback = h("div", {});
  const accountsPanel = h("div", { class: "openrouter-accounts", style: "display:none" });
  const toggleAccounts = h("button", { text: "View uploaded keys" });
  const addButton = h("button", { class: "primary", text: "+ Add account" });
  const addForm = h("div", { class: "openrouter-add-form", style: "display:none" });
  const nameField = h("input", {
    type: "text",
    placeholder: "Account name",
    maxlength: "80",
    autocomplete: "off",
    style: "flex:1 1 160px;min-width:0",
  }) as HTMLInputElement;
  const keyField = h("input", {
    type: "password",
    placeholder: "sk-or-v1-...",
    autocomplete: "off",
    spellcheck: "false",
    style: "flex:2 1 240px;min-width:0",
  }) as HTMLInputElement;
  const saveAccount = h("button", { text: "Save account" });
  addForm.append(
    h("div", { class: "row" },
      h("label", { text: "Account name" }),
      nameField,
      keyField,
      saveAccount,
    ),
  );

  function updateState() {
    dot.style.background = accounts.length || ollama.reachable ? "#22c55e" : "#f4505e";
    const ollamaText = ollama.reachable
      ? `Ollama ready (${ollama.models.length} model${ollama.models.length === 1 ? "" : "s"}).`
      : "Ollama unavailable.";
    const accountText = accounts.length
      ? `${accounts.length} OpenRouter account${accounts.length === 1 ? "" : "s"} stored securely.`
      : "No OpenRouter fallback key.";
    state.textContent = `${ollamaText} ${accountText}`;
    toggleAccounts.textContent = accountsPanel.style.display === "none"
      ? "View uploaded keys"
      : "Hide uploaded keys";
  }

  function renderAccounts() {
    clear(accountsPanel);
    for (const account of accounts) {
      const keyField = h("input", {
        type: "password",
        placeholder: "••••••••••••",
        readonly: true,
        autocomplete: "off",
        style: "flex:1 1 auto;min-width:0",
        "aria-label": `${account.name} API key`,
      }) as HTMLInputElement;
      const revealButton = h("button", { text: "👁", title: "Reveal API key", "aria-label": `Reveal ${account.name} API key` });
      let revealed = false;
      revealButton.addEventListener("click", async () => {
        clear(feedback);
        if (revealed) {
          keyField.value = "";
          keyField.type = "password";
          revealButton.title = "Reveal API key";
          revealed = false;
          return;
        }
        revealButton.disabled = true;
        try {
          keyField.value = await Bridge.openRouterAccountReveal(account.id);
          keyField.type = "text";
          revealButton.title = "Hide API key";
          revealed = true;
        } catch (err) {
          feedback.append(h("div", { class: "notice err", text: `Could not reveal key: ${String(err)}` }));
        } finally {
          revealButton.disabled = false;
        }
      });
      const removeButton = h("button", { class: "danger", text: "Remove" });
      removeButton.addEventListener("click", async () => {
        if (!confirm(`Remove the OpenRouter key for "${account.name}"?`)) return;
        removeButton.disabled = true;
        clear(feedback);
        try {
          await Bridge.openRouterAccountRemove(account.id);
          accounts = accounts.filter((item) => item.id !== account.id);
          renderAccounts();
          updateState();
          feedback.append(h("div", { class: "notice ok", text: "Account removed." }));
        } catch (err) {
          removeButton.disabled = false;
          feedback.append(h("div", { class: "notice err", text: `Could not remove account: ${String(err)}` }));
        }
      });
      accountsPanel.append(
        h("div", { class: "openrouter-account" },
          h("div", { class: "openrouter-account-name", text: account.name }),
          h("div", { class: "row openrouter-account-key" }, keyField, revealButton, removeButton),
        ),
      );
    }
  }

  toggleAccounts.addEventListener("click", async () => {
    clear(feedback);
    if (accountsPanel.style.display !== "none") {
      accountsPanel.style.display = "none";
      updateState();
      return;
    }
    try {
      accounts = await Bridge.openRouterAccounts();
      renderAccounts();
      accountsPanel.style.display = "";
      updateState();
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not load accounts: ${String(err)}` }));
    }
  });

  addButton.addEventListener("click", () => {
    addForm.style.display = addForm.style.display === "none" ? "" : "none";
    if (addForm.style.display !== "none") nameField.focus();
  });

  saveAccount.addEventListener("click", async () => {
    clear(feedback);
    if (!nameField.value.trim() || !keyField.value.trim()) {
      feedback.append(h("div", { class: "notice warn", text: "Enter both an account name and an API key." }));
      return;
    }
    saveAccount.disabled = true;
    try {
      await Bridge.openRouterAccountAdd(nameField.value, keyField.value);
      nameField.value = "";
      keyField.value = "";
      addForm.style.display = "none";
      accounts = await Bridge.openRouterAccounts();
      renderAccounts();
      if (accountsPanel.style.display !== "none") accountsPanel.style.display = "";
      updateState();
      feedback.append(h("div", { class: "notice ok", text: "Account saved securely in the OS credential manager." }));
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not save account: ${String(err)}` }));
    } finally {
      saveAccount.disabled = false;
    }
  });

  const model = h("select", {}) as HTMLSelectElement;
  for (const [id, label] of MODELS) model.append(h("option", { value: id, text: label }));
  if (!MODELS.some(([id]) => id === settings.model)) {
    model.append(h("option", { value: settings.model, text: settings.model }));
  }
  model.value = settings.model;
  model.addEventListener("change", () => {
    settings.model = model.value;
    void save();
  });

  const providerMode = h("select", {}) as HTMLSelectElement;
  providerMode.append(
    h("option", { value: "auto", text: "Auto — Ollama first, OpenRouter fallback" }),
    h("option", { value: "ollamaOnly", text: "Ollama only" }),
    h("option", { value: "openRouterOnly", text: "OpenRouter only" }),
  );
  providerMode.value = settings.providerMode;
  providerMode.addEventListener("change", () => {
    settings.providerMode = providerMode.value as Settings["providerMode"];
    void save();
  });

  const ollamaModel = h("select", {}) as HTMLSelectElement;
  const ollamaWarning = h("div", {});
  function renderOllamaModels() {
    const selected = settings.ollamaModel;
    const available = new Set(["qwen2.5:7b", "gpt-oss:20b", ...ollama.models]);
    if (!available.has(selected)) available.add(selected);
    clear(ollamaModel);
    for (const name of available) {
      ollamaModel.append(h("option", { value: name, text: name }));
    }
    ollamaModel.value = selected;
    clear(ollamaWarning);
    if (ollama.reachable && !ollama.models.includes(selected)) {
      ollamaWarning.append(h("div", {
        class: "notice warn",
        text: `Ollama model ${selected} is not installed. Run: ollama pull ${selected}`,
      }));
    } else if (!ollama.reachable) {
      ollamaWarning.append(h("div", {
        class: "notice warn",
        text: `Ollama is unavailable: ${ollama.error ?? "could not connect"}. Auto mode will use OpenRouter if configured.`,
      }));
    }
  }
  ollamaModel.addEventListener("change", () => {
    settings.ollamaModel = ollamaModel.value;
    renderOllamaModels();
    void save();
  });
  const refreshOllama = h("button", { text: "Check Ollama" });
  refreshOllama.addEventListener("click", async () => {
    refreshOllama.disabled = true;
    clear(feedback);
    try {
      ollama = await Bridge.ollamaStatus(true);
      renderOllamaModels();
      updateState();
    } catch (error) {
      feedback.append(h("div", {
        class: "notice err",
        text: `Could not check Ollama: ${String(error).replace(/^Error:\s*/, "")}`,
      }));
    } finally {
      refreshOllama.disabled = false;
    }
  });
  renderOllamaModels();

  renderAccounts();
  updateState();
  if (accountLoadError) {
    feedback.append(h("div", { class: "notice err", text: `Could not load accounts: ${accountLoadError}` }));
  }

  return h(
    "section",
    {},
    h("h2", {}, dot, h("span", { text: "LLM providers" })),
    h("div", { class: "row" }, h("label", { text: "Provider mode" }), providerMode),
    h("div", { class: "row" },
      h("label", { text: "Ollama model" }), ollamaModel, refreshOllama,
    ),
    ollamaWarning,
    h("p", {
      class: "hint",
      text: "Auto tries local Ollama first and uses OpenRouter only if Ollama is unavailable. Ollama-only never contacts OpenRouter.",
    }),
    h("h3", { text: "OpenRouter fallback" }),
    state,
    h("div", { class: "row" }, addButton, toggleAccounts),
    addForm,
    accountsPanel,
    h("div", { class: "row" }, h("label", { text: "Model" }), model),
    h("p", { class: "hint", text: "Automatic selects a suitable free model for each fallback prompt. You can choose any listed model to pin it instead. Some providers may retain prompts; avoid sending sensitive data to models you do not trust." }),
    feedback,
  );
}

// ── Integrations section ──────────────────────────────────────────────────────

interface IntegrationDef {
  id: string;
  name: string;
  color: string;
  /** Credential Manager keys, in the order they are shown. */
  fields: { key: string; label: string; placeholder: string; secret: boolean }[];
}

const INTEGRATIONS: IntegrationDef[] = [
  { id: "integration_stripe", name: "Stripe", color: "#0570DE",
    fields: [{ key: "stripe-api-key", label: "Secret key", placeholder: "sk_live_…", secret: true }] },
  { id: "integration_github", name: "GitHub", color: "#F4505E",
    fields: [{ key: "github-token", label: "Token", placeholder: "ghp_…", secret: true }] },
  { id: "integration_vercel", name: "Vercel", color: "#7C5CFF",
    fields: [{ key: "vercel-token", label: "Token", placeholder: "…", secret: true }] },
  { id: "integration_n8n", name: "n8n", color: "#F29B38",
    fields: [
      { key: "n8n-url", label: "Instance URL", placeholder: "https://n8n.example.com", secret: false },
      { key: "n8n-api-key", label: "API key", placeholder: "…", secret: true },
    ] },
  { id: "integration_resend", name: "Resend", color: "#22C55E",
    fields: [{ key: "resend-api-key", label: "API key", placeholder: "re_…", secret: true }] },
  { id: "integration_notion", name: "Notion", color: "#8C8C8C",
    fields: [{ key: "notion-api-key", label: "Integration token", placeholder: "ntn_…", secret: true }] },
  { id: "integration_calcom", name: "Cal.com", color: "#C9956A",
    fields: [{ key: "calcom-api-key", label: "API key", placeholder: "cal_…", secret: true }] },
];

const MAX_ACTIVE = 4;

function integrationsSection(present: Record<string, boolean>): HTMLElement {
  const note = h("div", { class: "hint" });
  const list = h("div", { style: "display:flex;flex-direction:column;gap:14px" });

  function updateNote() {
    const used = settings.activeIntegrations.length;
    note.textContent = `Pick up to ${MAX_ACTIVE} pills to show next to Mochi — ${used}/${MAX_ACTIVE} in use. Keys are stored in the Windows Credential Manager, never on disk.`;
  }

  for (const def of INTEGRATIONS) {
    const active = settings.activeIntegrations.includes(def.id);
    const sw = h("button", { class: active ? "switch on" : "switch" });
    sw.addEventListener("click", () => {
      const on = settings.activeIntegrations.includes(def.id);
      if (on) {
        settings.activeIntegrations = settings.activeIntegrations.filter((x) => x !== def.id);
      } else {
        if (settings.activeIntegrations.length >= MAX_ACTIVE) return;
        settings.activeIntegrations = [...settings.activeIntegrations, def.id];
      }
      sw.classList.toggle("on", !on);
      updateNote();
      void save();
    });

    const rows = h("div", { style: "display:flex;flex-direction:column;gap:6px;flex:1 1 auto;min-width:0" });
    for (const field of def.fields) {
      const input = h("input", {
        type: field.secret ? "password" : "text",
        placeholder: present[field.key] ? "••••••••  (stored)" : field.placeholder,
        autocomplete: "off",
        spellcheck: "false",
        style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const saveBtn = h("button", { text: "Save" });
      const dotEl = statusDot(present[field.key] ?? false);
      saveBtn.addEventListener("click", async () => {
        const value = input.value.trim();
        try {
          await Bridge.secretSet(field.key, value);
          present[field.key] = value.length > 0;
          input.value = "";
          input.placeholder = value ? "••••••••  (stored)" : field.placeholder;
          dotEl.style.background = value ? "#22c55e" : "#f4505e";
        } catch {
          dotEl.style.background = "#f5a524";
        }
      });
      rows.append(
        h("div", { class: "row" },
          h("label", { style: "min-width:104px", text: field.label }),
          input, saveBtn, dotEl,
        ),
      );
    }

    list.append(
      h("div", { style: "display:flex;gap:12px;align-items:flex-start" },
        h("div", { style: "display:flex;align-items:center;gap:8px;min-width:132px;padding-top:4px" },
          sw,
          h("i", { class: "dot", style: `background:${def.color}` }),
          h("span", { style: "font-size:12.5px", text: def.name }),
        ),
        rows,
      ),
    );
  }

  updateNote();
  return h("section", {}, h("h2", {}, h("span", { text: "Integrations" })), note, list);
}

// ── General section ───────────────────────────────────────────────────────────

function generalSection(): HTMLElement {
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005",
    value: String(settings.soundVolume),
  }) as HTMLInputElement;
  volume.addEventListener("input", () => {
    settings.soundVolume = Number(volume.value);
    void save();
  });

  const autoClose = h("input", {
    type: "number", min: "5", max: "120", step: "1",
    value: String(Math.round(settings.autoCloseInterval)),
    style: "width:72px",
  }) as HTMLInputElement;
  autoClose.addEventListener("change", () => {
    settings.autoCloseInterval = Math.max(5, Math.min(120, Number(autoClose.value) || 15));
    autoClose.value = String(settings.autoCloseInterval);
    void save();
  });

  const screen = h("select", {}) as HTMLSelectElement;
  screen.append(
    h("option", { value: "primary", text: "Main display" }),
    h("option", { value: "cursor", text: "Display under the cursor" }),
  );
  screen.value = settings.screen;
  screen.addEventListener("change", () => {
    settings.screen = screen.value as Settings["screen"];
    void save();
  });

  const speakMode = h("select", {}) as HTMLSelectElement;
  speakMode.append(
    h("option", { value: "off", text: "Off" }),
    h("option", { value: "voiceOnly", text: "Voice messages only" }),
    h("option", { value: "always", text: "Always" }),
  );
  speakMode.value = settings.speakRepliesMode;
  speakMode.addEventListener("change", () => {
    settings.speakRepliesMode = speakMode.value as Settings["speakRepliesMode"];
    void save();
  });

  const wakePhraseHint = h("span", {
    class: "hint",
    text: `Saved phrase: “${settings.wakeWordPronunciation}”.`,
  });
  const calibrateWakePhrase = h("button", { text: "Record “Hey Macha”" });
  calibrateWakePhrase.addEventListener("click", async () => {
    calibrateWakePhrase.disabled = true;
    wakePhraseHint.textContent = "Recording for 5 seconds — say “Hey Macha” a few times.";
    try {
      await Bridge.setWakeCalibration(true);
      await Bridge.setVoiceActive(true);
      const calibration = await recordWakePronunciation();
      settings.wakeWordPronunciation = calibration.transcript;
      settings.wakeWordThreshold = calibration.threshold;
      State.settings = { ...State.settings, ...settings };
      await Bridge.saveSettingsStrict(settings);
      wakePhraseHint.textContent = `Saved “${calibration.transcript}” and tuned microphone sensitivity. The recording was discarded.`;
    } catch (error) {
      wakePhraseHint.textContent = error instanceof Error
        ? error.message
        : "Could not calibrate the wake phrase.";
      console.error("[coucou] wake phrase calibration failed", error);
    } finally {
      try {
        await Bridge.setVoiceActive(false);
        await Bridge.setWakeCalibration(false);
      } catch (error) {
        console.error("[coucou] could not restore the wake listener after calibration", error);
      }
      calibrateWakePhrase.disabled = false;
    }
  });

  const engine = h("select", {}) as HTMLSelectElement;
  engine.append(
    h("option", { value: "auto", text: "Automatic (recommended)" }),
    h("option", { value: "webSpeech", text: "Web Speech (local voices)" }),
    h("option", { value: "sapi", text: "Windows SAPI" }),
  );
  engine.value = settings.ttsEngine;
  engine.addEventListener("change", () => {
    settings.ttsEngine = engine.value as Settings["ttsEngine"];
    void save();
  });

  const voice = h("select", {}) as HTMLSelectElement;
  voice.append(h("option", { value: "", text: "Automatic — preferred local voice" }));
  voice.value = settings.ttsVoice;
  voice.addEventListener("change", () => {
    settings.ttsVoice = voice.value;
    void save();
  });
  void TTS.availableVoices().then((voices) => {
    if (settings.ttsVoice && !voices.some((item) => item.voiceURI === settings.ttsVoice)) {
      voice.append(h("option", {
        value: settings.ttsVoice,
        text: `${settings.ttsVoice} (unavailable or not English)`,
      }));
    }
    for (const item of voices) {
      voice.append(h("option", { value: item.voiceURI, text: `${item.name} (${item.lang})` }));
    }
    voice.value = settings.ttsVoice;
  }).catch((error: unknown) => {
    console.error("[coucou] could not list local speech voices", error);
  });

  const rateLabel = h("span", { text: `${settings.ttsRate.toFixed(1)}×` });
  const rate = h("input", {
    type: "range", min: "0.8", max: "1.4", step: "0.1", value: String(settings.ttsRate),
  }) as HTMLInputElement;
  rate.addEventListener("input", () => {
    settings.ttsRate = Number(rate.value);
    rateLabel.textContent = `${settings.ttsRate.toFixed(1)}×`;
  });
  rate.addEventListener("change", () => void save());

  const ttsVolumeLabel = h("span", { text: `${Math.round(settings.ttsVolume * 100)}%` });
  const ttsVolume = h("input", {
    type: "range", min: "0", max: "1", step: "0.05", value: String(settings.ttsVolume),
  }) as HTMLInputElement;
  ttsVolume.addEventListener("input", () => {
    settings.ttsVolume = Number(ttsVolume.value);
    ttsVolumeLabel.textContent = `${Math.round(settings.ttsVolume * 100)}%`;
  });
  ttsVolume.addEventListener("change", () => void save());

  const testVoice = h("button", { text: "Test voice" });
  const voiceFeedback = h("span", { class: "hint" });
  TTS.onStateChange((speaking, error) => {
    if (error) voiceFeedback.textContent = error;
    else if (speaking) voiceFeedback.textContent = "Speaking…";
    else voiceFeedback.textContent = "";
  });
  testVoice.addEventListener("click", () => {
    void save();
    void TTS.speak("Hi, I'm coucou.");
  });

  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: "General" })),
    h("div", { class: "row" },
      h("label", { text: "Sound" }),
      toggle(settings.soundEnabled, (v) => { settings.soundEnabled = v; void save(); }),
      volume,
    ),
    h("div", { class: "row" },
      h("label", { text: "Auto-close" }),
      autoClose,
      h("span", { class: "hint", text: "seconds after you leave the island" }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Island lives on" }),
      screen,
    ),
    h("div", { class: "row" },
      h("label", { text: "Launch at startup" }),
      toggle(settings.autostart, (v) => { settings.autostart = v; void save(); }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Speak replies" }),
      speakMode,
    ),
    h("div", { class: "row" },
      h("label", { text: 'Wake phrase "Hey Macha"' }),
      toggle(settings.wakeWordEnabled, (enabled) => {
        settings.wakeWordEnabled = enabled;
        void save();
      }),
      h("span", { class: "hint", text: "Listen locally for Hey Macha; speech audio is sent only to your local Whisper service." }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Your pronunciation" }),
      calibrateWakePhrase,
      wakePhraseHint,
    ),
    h("div", { class: "row" },
      h("label", { text: "Speech engine" }),
      engine,
    ),
    h("div", { class: "row" },
      h("label", { text: "Local voice" }),
      voice,
    ),
    h("div", { class: "row" },
      h("label", { text: "Speech rate" }),
      rate,
      rateLabel,
    ),
    h("div", { class: "row" },
      h("label", { text: "Speech volume" }),
      ttsVolume,
      ttsVolumeLabel,
    ),
    h("div", { class: "row" },
      h("label", { text: "Test" }),
      testVoice,
      voiceFeedback,
    ),
  );
}

function automationSection(): HTMLElement {
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", {});

  function drawFolders() {
    clear(list);
    if (settings.automationFolders.length === 0) {
      list.append(h("div", {
        class: "hint",
        text: "No folders are authorized. Mochi cannot inspect or change local files until you add one.",
      }));
      return;
    }
    for (const folder of settings.automationFolders) {
      const remove = h("button", {
        class: "danger",
        text: "Remove",
        "aria-label": `Remove ${folder}`,
        onclick: async () => {
          const previous = [...settings.automationFolders];
          settings.automationFolders = settings.automationFolders.filter((path) => path !== folder);
          try {
            await Bridge.saveSettingsStrict(settings);
            drawFolders();
          } catch (err) {
            settings.automationFolders = previous;
            feedback.replaceChildren(h("div", {
              class: "notice err",
              text: `Could not save authorized folders: ${String(err)}`,
            }));
          }
        },
      });
      list.append(h("div", { class: "row" },
        h("span", { class: "path", style: "flex:1 1 300px", text: folder }),
        remove,
      ));
    }
  }

  const add = h("button", {
    class: "primary",
    text: "Choose folder…",
    onclick: async () => {
      const previous = [...settings.automationFolders];
      try {
        const folder = await Bridge.pickAutomationFolder();
        if (!folder || settings.automationFolders.includes(folder)) return;
        settings.automationFolders.push(folder);
        await Bridge.saveSettingsStrict(settings);
        feedback.replaceChildren();
        drawFolders();
      } catch (err) {
        settings.automationFolders = previous;
        feedback.replaceChildren(h("div", {
          class: "notice err",
          text: `Could not authorize folder: ${String(err)}`,
        }));
      }
    },
  });

  drawFolders();
  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: "Local automation" })),
    h("div", {
      class: "hint",
      text: "Mochi can open installed apps and work with files only inside these folders. Each action needs your approval. File edits show a preview and create a backup first. Deleting files and running commands are not available. Files you ask Mochi to read are sent to the selected chat provider.",
    }),
    list,
    h("div", { class: "row" }, add),
    feedback,
  );
}

// ── Boot ──────────────────────────────────────────────────────────────────────

async function main() {
  const boot = await Bridge.boot();
  if (boot) {
    settings = { ...settings, ...boot.settings };
    State.settings = { ...State.settings, ...boot.settings };
    version = boot.version;
  }
  const status = (await Bridge.hooksStatus()) ?? {
    installed: false, settingsPath: "", hookPath: "", hookReady: false,
  };

  let openRouterAccounts: OpenRouterAccount[] = [];
  let openRouterError: string | undefined;
  let ollamaStatus: OllamaStatus = { reachable: false, models: [], error: "Ollama status has not been checked." };
  try {
    openRouterAccounts = await Bridge.openRouterAccounts();
  } catch (err) {
    openRouterError = String(err);
  }
  try {
    ollamaStatus = await Bridge.ollamaStatus();
  } catch (err) {
    ollamaStatus = { reachable: false, models: [], error: String(err).replace(/^Error:\s*/, "") };
  }

  const keys = [
    "stripe-api-key", "github-token", "vercel-token",
    "n8n-url", "n8n-api-key", "resend-api-key", "notion-api-key", "calcom-api-key",
  ];
  const present: Record<string, boolean> = {};
  for (const k of keys) present[k] = (await Bridge.secretPresent(k)) ?? false;

  clear(root);
  root.append(
    h("h1", {}, h("span", { text: "Coucou" }), h("span", { class: "version", text: version })),
    claudeSection(status),
    apiSection(openRouterAccounts, ollamaStatus, openRouterError),
    automationSection(),
    integrationsSection(present),
    generalSection(),
    h("div", {
      class: "hint",
      text: "No telemetry. Network requests only go to the services you configure yourself.",
    }),
  );

  void onEvent<Settings>("settings-changed", (s) => {
    settings = { ...settings, ...s };
    State.settings = { ...State.settings, ...s };
  });
}

void main();
