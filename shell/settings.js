/* Persea Desktop settings page (D02): instances CRUD + probe display,
 * appearance (shell theme), placeholders for later tickets, About.
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
    return {
      text: known
        ? "Unreachable — last known version " + probe.version
        : "Unreachable — never reached",
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
    invoke("instances_set_default", { url: inst.url })
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
    invoke("instances_open_setup", { url: e.currentTarget.dataset.openSetup }).catch(() => {});
  });
  row.appendChild(main);

  const actions = document.createElement("div");
  actions.className = "instance-actions";

  const openBtn = document.createElement("button");
  openBtn.type = "button";
  openBtn.className = "btn btn-accent";
  openBtn.textContent = "Open";
  openBtn.addEventListener("click", () => {
    invoke("instances_open", { url: inst.url }).catch((err) => alert("Could not open: " + err));
  });
  actions.appendChild(openBtn);

  const recheckBtn = document.createElement("button");
  recheckBtn.type = "button";
  recheckBtn.className = "btn btn-ghost";
  recheckBtn.textContent = "Recheck";
  recheckBtn.addEventListener("click", () => {
    recheckBtn.disabled = true;
    recheckBtn.textContent = "Checking…";
    invoke("instances_probe", { url: inst.url })
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
      invoke("instances_remove", { url: inst.url })
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
    instances = await invoke("instances_list");
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
  dialogDesc.textContent = "Changing the URL re-checks the server. Its data store keeps the previous URL's cookies.";
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
      await invoke("instances_update", { url: editingUrl, name, newUrl: url });
    } else {
      await invoke("instances_add", { name, url });
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
    settings = await invoke("shell_get_settings");
  } catch {
    settings = null;
  }
  applyAppearanceSetting((settings && settings.appearance) || "auto");

  document.getElementById("appearance-group").addEventListener("change", (event) => {
    const value = event.target.value;
    if (!value) return;
    applyAppearanceSetting(value);
    invoke("shell_set_appearance", { appearance: value }).catch(() => {});
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
    invoke("instances_open_default").catch((err) => alert("No server to open: " + err));
  });
}

reloadInstances();
initAppearance();
initClipboard();
initAbout();
initHeader();
