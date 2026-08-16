/* Persea Desktop settings page: instances CRUD + probe display,
 * appearance (shell theme), hardware acceleration, global shortcuts,
 * placeholders for later features, About.
 * Runs after app.js (invoke, initTheme, capabilityChips, copyText).
 */

const listEl = document.getElementById("instance-list");
const dialog = document.getElementById("instance-dialog");
const dialogTitle = document.getElementById("instance-dialog-title");
const dialogDesc = document.getElementById("instance-dialog-desc");
const instanceForm = document.getElementById("instance-form");
const instanceName = document.getElementById("instance-name");
const instanceUrl = document.getElementById("instance-url");

let editingUrl = null;

/* ------------------------------------------------------------------ */
/* Instance list                                                      */
/* ------------------------------------------------------------------ */

function statusLine(inst) {
  const probe = inst.probe;
  if (!probe) {
    return { text: "Not checked yet", cls: "" };
  }
  if (probe.needsSetup) {
    return { text: "This server needs setup", cls: "warn" };
  }
  if (!probe.ok) {
    const known = probe.version && probe.version !== "unknown";
    const detail = probe.error
      ? probe.error
      : known
        ? "last known version " + probe.version
        : "never reached";
    return {
      text: "Unreachable — " + detail,
      cls: "offline",
    };
  }
  return { text: "Server " + probe.version, cls: "ok" };
}

function statusBlock(inst) {
  const probe = inst.probe;
  const parts = [];
  const line = statusLine(inst);
  parts.push('<div class="instance-status ' + escapeHtml(line.cls) + '">' + escapeHtml(line.text) + "</div>");

  if (!probe) return parts.join("");

  if (probe.needsSetup) {
    parts.push(
      '<div class="setup-banner">' +
        '<span>This server has not been set up yet.</span>' +
        '<button type="button" class="btn btn-accent" data-open-setup="' +
        escapeHtml(inst.url) +
        '">Open setup</button></div>'
    );
  }

  if (probe.updateAvailable && probe.latestVersion) {
    parts.push(
      '<div class="update-note">Server update available: ' +
        escapeHtml(probe.latestVersion) +
        "</div>"
    );
  }

  const chips = capabilityChips(probe.capabilities);
  if (chips.length) {
    parts.push('<div class="cap-chips">' + chips.join("") + "</div>");
  }

  if (probe.warnings && probe.warnings.length) {
    parts.push(
      '<ul class="status-warnings">' +
        probe.warnings.map((w) => "<li>" + escapeHtml(w) + "</li>").join("") +
        "</ul>"
    );
  }

  return parts.join("");
}

function renderInstanceRow(inst) {
  const row = document.createElement("div");
  row.className = "instance-row";
  row.setAttribute("data-url", inst.url);

  const main = document.createElement("div");
  main.className = "instance-main";

  const nameLine = document.createElement("div");
  nameLine.className = "instance-name-line";
  const name = document.createElement("span");
  name.className = "instance-name";
  name.textContent = inst.name;
  nameLine.appendChild(name);

  const defLabel = document.createElement("label");
  defLabel.className = "instance-default";
  const defRadio = document.createElement("input");
  defRadio.type = "radio";
  defRadio.name = "default-instance";
  defRadio.checked = inst.default;
  defRadio.disabled = inst.locked;
  defRadio.setAttribute("aria-label", "Set " + inst.name + " as the default server");
  defRadio.addEventListener("change", () => {
    invoke("cmd_instances_set_default", { url: inst.url })
      .then(() => reloadInstances())
      .catch((err) => alert("Could not set default: " + err));
  });
  defLabel.appendChild(defRadio);
  defLabel.appendChild(document.createTextNode("Default"));
  nameLine.appendChild(defLabel);
  main.appendChild(nameLine);

  const url = document.createElement("div");
  url.className = "instance-url";
  url.textContent = inst.url;
  main.appendChild(url);

  main.insertAdjacentHTML("beforeend", statusBlock(inst));
  main.querySelector("[data-open-setup]")?.addEventListener("click", (e) => {
    e.preventDefault();
    invoke("cmd_instances_open_setup", { url: e.currentTarget.dataset.openSetup }).catch(() => {});
  });
  row.appendChild(main);

  const actions = document.createElement("div");
  actions.className = "instance-actions";

  const openBtn = document.createElement("button");
  openBtn.type = "button";
  openBtn.className = "btn btn-accent";
  openBtn.textContent = "Open";
  openBtn.addEventListener("click", () => {
    invoke("cmd_instances_open", { url: inst.url }).catch((err) => alert("Could not open: " + err));
  });
  actions.appendChild(openBtn);

  const recheckBtn = document.createElement("button");
  recheckBtn.type = "button";
  recheckBtn.className = "btn btn-ghost";
  recheckBtn.textContent = "Recheck";
  recheckBtn.addEventListener("click", () => {
    recheckBtn.disabled = true;
    recheckBtn.textContent = "Checking…";
    invoke("cmd_instances_probe", { url: inst.url })
      .then(() => reloadInstances())
      .catch((err) => alert("Probe failed: " + err))
      .finally(() => {
        recheckBtn.disabled = false;
        recheckBtn.textContent = "Recheck";
      });
  });
  actions.appendChild(recheckBtn);

  if (!inst.locked) {
    const editBtn = document.createElement("button");
    editBtn.type = "button";
    editBtn.className = "btn btn-ghost";
    editBtn.textContent = "Edit";
    editBtn.addEventListener("click", () => openEditDialog(inst));
    actions.appendChild(editBtn);

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "btn btn-danger";
    removeBtn.textContent = "Remove";
    removeBtn.addEventListener("click", () => {
      if (!confirm("Remove the server \"" + inst.name + "\" from this app? Its stored login is left on disk.")) {
        return;
      }
      invoke("cmd_instances_remove", { url: inst.url })
        .then(() => reloadInstances())
        .catch((err) => alert("Could not remove: " + err));
    });
    actions.appendChild(removeBtn);
  }

  row.appendChild(actions);
  return row;
}

async function reloadInstances() {
  let instances = [];
  try {
    instances = await invoke("cmd_instances_list");
  } catch {
    instances = [];
  }
  listEl.textContent = "";
  if (!instances.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "No servers configured yet. Add one to get started.";
    listEl.appendChild(empty);
    return;
  }
  instances
    .map(renderInstanceRow)
    .forEach((row) => listEl.appendChild(row));
}

/* ------------------------------------------------------------------ */
/* Add / edit dialog                                                  */
/* ------------------------------------------------------------------ */

function openAddDialog() {
  editingUrl = null;
  dialogTitle.textContent = "Add server";
  dialogDesc.textContent = "The app checks the server and shows its version and capabilities.";
  instanceForm.reset();
  instanceForm.dataset.mode = "add";
  dialog.showModal();
  instanceName.focus();
}

function openEditDialog(inst) {
  editingUrl = inst.url;
  dialogTitle.textContent = "Edit server";
  dialogDesc.textContent =
    "Changing the URL re-checks the server. Its data store keeps the previous URL's cookies, and device pairing is tied to the URL, so a renamed server must be paired again.";
  instanceName.value = inst.name;
  instanceUrl.value = inst.url;
  instanceForm.dataset.mode = "edit";
  dialog.showModal();
  instanceName.focus();
}

instanceForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const name = instanceName.value.trim();
  const url = instanceUrl.value.trim();
  const saveBtn = instanceForm.querySelector('[type="submit"]');
  saveBtn.disabled = true;
  try {
    if (instanceForm.dataset.mode === "edit" && editingUrl) {
      await invoke("cmd_instances_update", { url: editingUrl, name, newUrl: url });
    } else {
      await invoke("cmd_instances_add", { name, url });
    }
    dialog.close();
    await reloadInstances();
  } catch (err) {
    dialogDesc.textContent = "Could not save: " + err;
  } finally {
    saveBtn.disabled = false;
  }
});

document.getElementById("btn-add-instance").addEventListener("click", openAddDialog);
document.getElementById("instance-dialog-cancel").addEventListener("click", () => dialog.close());

/* ------------------------------------------------------------------ */
/* Appearance                                                         */
/* ------------------------------------------------------------------ */

function applyAppearanceSetting(appearance) {
  const group = document.getElementById("appearance-group");
  const radio = group.querySelector('input[value="' + escapeHtml(appearance) + '"]');
  if (radio) radio.checked = true;
  const htmlClass = document.documentElement.classList;
  htmlClass.toggle("light", appearance === "light");
  htmlClass.toggle("dark", appearance === "dark");
}

async function initAppearance() {
  let settings = null;
  try {
    settings = await invoke("cmd_shell_get_settings");
  } catch {
    settings = null;
  }
  applyAppearanceSetting((settings && settings.appearance) || "auto");

  document.getElementById("appearance-group").addEventListener("change", (event) => {
    const value = event.target.value;
    if (!value) return;
    applyAppearanceSetting(value);
    invoke("cmd_shell_set_appearance", { appearance: value }).catch(() => {});
  });
}

/* ------------------------------------------------------------------ */
/* Clipboard (pairing placeholder)                                    */
/* ------------------------------------------------------------------ */

function initClipboard() {
  const copyBtn = document.getElementById("btn-copy-code");
  const code = document.getElementById("pairing-code");
  if (!copyBtn || !code) return;
  copyBtn.addEventListener("click", async () => {
    const ok = await copyText(code.value);
    copyBtn.textContent = ok ? "Copied" : "Copy failed";
    setTimeout(() => {
      copyBtn.textContent = "Copy";
    }, 1500);
  });
}

/* ------------------------------------------------------------------ */
/* Shortcuts                                                          */
/* ------------------------------------------------------------------ */

const SHORTCUT_STATUS_LABELS = {
  registered: "Active",
  conflict: "Conflict",
  unavailable: "Unavailable",
  disabled: "Disabled",
};

function shortcutStatusClass(status) {
  switch (status) {
    case "registered":
      return "ok";
    case "conflict":
      return "warn";
    case "unavailable":
    case "disabled":
      return "offline";
    default:
      return "";
  }
}

function renderShortcutRow(entry, editable) {
  const row = document.createElement("div");
  row.className = "shortcut-row";

  const main = document.createElement("div");
  main.className = "shortcut-main";

  const title = document.createElement("div");
  title.className = "shortcut-title";
  title.textContent = entry.label;
  main.appendChild(title);

  const desc = document.createElement("div");
  desc.className = "shortcut-desc";
  desc.textContent = entry.description;
  main.appendChild(desc);

  const inputRow = document.createElement("div");
  inputRow.className = "shortcut-input-row";

  const input = document.createElement("input");
  input.type = "text";
  input.value = entry.shortcut;
  input.className = "shortcut-input";
  input.spellcheck = false;
  input.disabled = !editable;
  input.setAttribute("aria-label", "Shortcut chord for " + entry.label);
  inputRow.appendChild(input);

  const status = document.createElement("span");
  status.className = "shortcut-status " + shortcutStatusClass(entry.status);
  status.textContent = SHORTCUT_STATUS_LABELS[entry.status] || entry.status;
  inputRow.appendChild(status);

  const saveBtn = document.createElement("button");
  saveBtn.type = "button";
  saveBtn.className = "btn btn-ghost";
  saveBtn.textContent = "Save";
  saveBtn.disabled = !editable;
  const save = async () => {
    saveBtn.disabled = true;
    try {
      const view = await invoke("cmd_hotkeys_set_shortcut", {
        id: entry.id,
        shortcut: input.value.trim(),
      });
      renderShortcutsView(view);
    } catch (err) {
      alert("Could not change shortcut: " + err);
    } finally {
      saveBtn.disabled = false;
    }
  };
  saveBtn.addEventListener("click", save);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      save();
    }
  });
  inputRow.appendChild(saveBtn);

  main.appendChild(inputRow);
  row.appendChild(main);
  return row;
}

function renderShortcutsView(view) {
  const noteEl = document.getElementById("shortcuts-note");
  const listEl = document.getElementById("shortcuts-list");
  if (!noteEl || !listEl) return;

  const notes = [];
  if (!view.platformSupported) {
    notes.push(
      "Global shortcuts are unavailable on Wayland; the app stays fully " +
        "functional. Set window and session keybindings in your compositor instead."
    );
  }
  if (view.enabled === false) {
    notes.push("Shortcuts are disabled while kiosk mode is active.");
  }
  if (view.shortcuts.some((s) => s.status === "conflict")) {
    notes.push(
      "A chord could not be registered: the OS or another program already " +
        "uses it. Pick a different chord; no fallback is applied."
    );
  }
  noteEl.textContent = notes.join(" ");
  noteEl.classList.toggle("hidden", notes.length === 0);

  listEl.textContent = "";
  const editable = view.platformSupported && view.enabled !== false;
  view.shortcuts.forEach((entry) => listEl.appendChild(renderShortcutRow(entry, editable)));
}

async function initShortcuts() {
  const listEl = document.getElementById("shortcuts-list");
  if (!listEl) return;
  let view = null;
  try {
    view = await invoke("cmd_hotkeys_get_settings");
  } catch {
    view = null;
  }
  if (!view) {
    listEl.textContent = "Shortcut status is unavailable right now.";
    return;
  }
  renderShortcutsView(view);
}

/* ------------------------------------------------------------------ */
/* Performance (hardware acceleration)                                 */
/* ------------------------------------------------------------------ */

async function initGpuAcceleration() {
  const toggle = document.getElementById("gpu-acceleration-enabled");
  if (!toggle) return;
  let settings = null;
  try {
    settings = await invoke("cmd_shell_get_settings");
  } catch {
    return;
  }
  // Unset (no gpuAcceleration in shell.json yet) = engine defaults = on.
  toggle.checked = settings && settings.gpuAcceleration !== false;
  toggle.addEventListener("change", async () => {
    try {
      await invoke("cmd_shell_set_gpu_acceleration", { enabled: toggle.checked });
    } catch (err) {
      toggle.checked = !toggle.checked;
      alert("Failed to update hardware acceleration: " + err);
    }
  });
}

/* ------------------------------------------------------------------ */
/* Network (untrusted TLS certificates)                                */
/* ------------------------------------------------------------------ */

async function initInsecureTls() {
  const toggle = document.getElementById("insecure-tls-enabled");
  if (!toggle) return;
  let settings = null;
  try {
    settings = await invoke("cmd_shell_get_settings");
  } catch {
    return;
  }
  toggle.checked = !!(settings && settings.allowInsecureTls);
  toggle.addEventListener("change", async () => {
    try {
      await invoke("cmd_shell_set_insecure_tls", { enabled: toggle.checked });
    } catch (err) {
      toggle.checked = !toggle.checked;
      alert("Failed to update the TLS setting: " + err);
    }
  });
}

/* ------------------------------------------------------------------ */
/* About + header                                                     */
/* ------------------------------------------------------------------ */

async function initAbout() {
  const versionEl = document.getElementById("about-version");
  if (versionEl) versionEl.textContent = await appVersion();
}

async function initHeader() {
  const openDefault = document.getElementById("btn-open-default");
  if (!openDefault) return;
  openDefault.addEventListener("click", () => {
    invoke("cmd_instances_open_default").catch((err) => alert("No server to open: " + err));
  });
}

async function initNotifications() {
  const toggle = document.getElementById("notifications-enabled");
  if (!toggle) return;
  try {
    toggle.checked = await invoke("notifications_get_enabled");
  } catch {
    return;
  }
  toggle.addEventListener("change", async () => {
    try {
      await invoke("notifications_set_enabled", { enabled: toggle.checked });
    } catch (err) {
      toggle.checked = !toggle.checked;
      alert("Failed to update notifications: " + err);
    }
  });
}

reloadInstances();
initAppearance();
initClipboard();
initShortcuts();
initAbout();
initHeader();
initNotifications();
initGpuAcceleration();
initInsecureTls();
