/* Persea Desktop device pairing page.
 *
 * The pairing flow lives Rust-side (pairing.rs); this page renders the
 * modal, the code, and the paired-token list, and drives the Tauri
 * commands: pairing_supported, pairing_start, pairing_status,
 * pairing_cancel, pairing_open_confirm_page, pairing_list_tokens,
 * pairing_revoke. Runs after app.js (invoke, escapeHtml, copyText).
 *
 * Capability gate: when the instance probe does not report the
 * desktop_pairing capability, the pairing UI is hidden entirely and only
 * the disabled notice shows.
 *
 * Instance selection: ?url=<instance url> opens the page for a specific
 * instance (the settings entry point links this way); without the
 * parameter the default instance is used.
 *
 * The modal survives navigation: "Open pairing page" navigates the main
 * window to the server, and the Rust poll loop keeps running. When the
 * user comes back to this page, an in-flight pairing reopens the modal
 * with the live state.
 */

const UI_POLL_MS = 1000;

let instanceUrl = null;
let uiTimer = null;
let currentCode = null;

const dialog = document.getElementById("pairing-dialog");
const codeEl = document.getElementById("pairing-code");
const copyBtn = document.getElementById("btn-copy-code");
const statusEl = document.getElementById("pairing-status");
const openPageBtn = document.getElementById("btn-open-pairing-page");
const cancelBtn = document.getElementById("pairing-cancel");
const retryBtn = document.getElementById("btn-pair-retry");
const pairBtn = document.getElementById("btn-pair");
const pairedList = document.getElementById("paired-list");
const disabledBox = document.getElementById("pairing-disabled");
const descEl = document.getElementById("pairing-desc");

/* ------------------------------------------------------------------ */
/* Instance resolution                                                */
/* ------------------------------------------------------------------ */

function queryInstanceUrl() {
  return new URLSearchParams(window.location.search).get("url");
}

async function resolveInstance() {
  const wanted = queryInstanceUrl();
  let instances = [];
  try {
    instances = await invoke("cmd_instances_list");
  } catch {
    instances = [];
  }
  if (!instances.length) return null;
  if (wanted) {
    return instances.find((i) => i.url === wanted) || instances[0];
  }
  return instances.find((i) => i.default) || instances[0];
}

/* ------------------------------------------------------------------ */
/* Dialog rendering                                                   */
/* ------------------------------------------------------------------ */

function stopUiPoll() {
  if (uiTimer) {
    clearInterval(uiTimer);
    uiTimer = null;
  }
}

function closeDialog() {
  stopUiPoll();
  dialog.close();
}

function remainingText(expiresAt) {
  const target = Date.parse(expiresAt);
  if (Number.isNaN(target)) return "";
  const secs = Math.max(0, Math.floor((target - Date.now()) / 1000));
  const minutes = Math.floor(secs / 60);
  const seconds = String(secs % 60).padStart(2, "0");
  return minutes + "m " + seconds + "s";
}

function showCode(code) {
  currentCode = code;
  const grouped = code.length > 4 ? code.slice(0, 4) + "  " + code.slice(4) : code;
  codeEl.textContent = grouped;
  codeEl.classList.remove("hidden");
  copyBtn.classList.remove("hidden");
  openPageBtn.classList.remove("hidden");
  retryBtn.classList.add("hidden");
  cancelBtn.classList.remove("hidden");
  cancelBtn.textContent = "Cancel";
}

function showWaiting(state) {
  showCode(state.code);
  const countdown = remainingText(state.expiresAt);
  statusEl.textContent =
    "Waiting for approval" +
    (countdown ? " — " + countdown : "") +
    ". Confirm the code on the server's account page to finish pairing.";
}

function showTerminal(message, withRetry) {
  currentCode = null;
  codeEl.classList.add("hidden");
  copyBtn.classList.add("hidden");
  openPageBtn.classList.add("hidden");
  cancelBtn.textContent = "Close";
  retryBtn.classList.toggle("hidden", !withRetry);
  statusEl.textContent = message;
}

function renderDialogState(state) {
  if (!state) return;
  switch (state.status) {
    case "waiting":
      showWaiting(state);
      break;
    case "approved":
      showTerminal(
        "Paired. The device token \"" + state.tokenName + "\" is stored in the OS keychain."
      );
      break;
    case "expired":
      showTerminal(
        "The code expired before it was confirmed (codes last 10 minutes). Try again for a new code.",
        true
      );
      break;
    case "used":
      showTerminal("This code was already used. Try again for a new code.", true);
      break;
    case "timedOut":
      showTerminal("Pairing timed out after 10 minutes. Try again.", true);
      break;
    case "failed":
      showTerminal(state.message || "Pairing failed.", true);
      break;
    case "cancelled":
      showTerminal("Pairing cancelled.");
      break;
    case "idle":
      showTerminal("No pairing is in progress.");
      break;
    default:
      showTerminal("Unexpected pairing state: " + escapeHtml(state.status));
  }
}

async function pollDialogUi() {
  try {
    const state = await invoke("pairing_status", { instanceUrl: instanceUrl });
    renderDialogState(state);
    return state;
  } catch {
    return null;
  }
}

async function startPairing() {
  stopUiPoll();
  dialog.showModal();
  currentCode = null;
  statusEl.textContent = "Contacting the server…";
  codeEl.classList.add("hidden");
  copyBtn.classList.add("hidden");
  openPageBtn.classList.add("hidden");
  retryBtn.classList.add("hidden");
  cancelBtn.textContent = "Cancel";
  cancelBtn.classList.remove("hidden");
  try {
    const state = await invoke("pairing_start", { instanceUrl: instanceUrl });
    renderDialogState(state);
  } catch (err) {
    renderDialogState({ status: "failed", message: String(err) });
  }
  uiTimer = setInterval(pollDialogUi, UI_POLL_MS);
}

/* Reopens the modal when a pairing is already in flight (the user
 * navigated to the server and back while the poll loop ran). */
async function resumePairingIfActive() {
  const state = await pollDialogUi();
  if (state && state.status === "waiting") {
    dialog.showModal();
    uiTimer = setInterval(pollDialogUi, UI_POLL_MS);
  }
}

/* ------------------------------------------------------------------ */
/* Paired token list                                                  */
/* ------------------------------------------------------------------ */

function renderTokenRow(token) {
  const row = document.createElement("div");
  row.className = "instance-row";

  const main = document.createElement("div");
  main.className = "instance-main";
  const name = document.createElement("div");
  name.className = "instance-name";
  name.textContent = token.tokenName || "Paired device";
  const meta = document.createElement("div");
  meta.className = "instance-url";
  meta.textContent =
    "Created " +
    new Date(token.createdAt * 1000).toLocaleString() +
    (token.inKeychain
      ? " — stored in the OS keychain"
      : " — secret missing, re-pair this device");
  main.appendChild(name);
  main.appendChild(meta);
  row.appendChild(main);

  const actions = document.createElement("div");
  actions.className = "instance-actions";
  const revoke = document.createElement("button");
  revoke.type = "button";
  revoke.className = "btn btn-danger";
  revoke.textContent = "Revoke";
  revoke.addEventListener("click", async () => {
    if (
      !confirm(
        "Revoke the device token \"" + token.tokenName + "\"? The shell will stop using it immediately."
      )
    ) {
      return;
    }
    try {
      await invoke("pairing_revoke", {
        instanceUrl: instanceUrl,
        tokenId: token.tokenId,
      });
      await renderTokens();
    } catch (err) {
      alert("Revocation failed: " + err);
    }
  });
  actions.appendChild(revoke);
  row.appendChild(actions);
  return row;
}

async function renderTokens() {
  let tokens = [];
  try {
    tokens = await invoke("pairing_list_tokens", { instanceUrl: instanceUrl });
  } catch {
    tokens = [];
  }
  pairedList.textContent = "";
  if (!tokens.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent =
      "No device tokens yet. Pairing lets the shell talk to the server with the identity of the signed-in user.";
    pairedList.appendChild(empty);
    return;
  }
  tokens.map(renderTokenRow).forEach((row) => pairedList.appendChild(row));
}

/* ------------------------------------------------------------------ */
/* Wiring + init                                                      */
/* ------------------------------------------------------------------ */

pairBtn.addEventListener("click", startPairing);

copyBtn.addEventListener("click", async () => {
  if (!currentCode) return;
  const ok = await copyText(currentCode);
  copyBtn.textContent = ok ? "Copied" : "Copy failed";
  setTimeout(() => {
    copyBtn.textContent = "Copy";
  }, 1500);
});

openPageBtn.addEventListener("click", () => {
  invoke("pairing_open_confirm_page", { instanceUrl: instanceUrl }).catch((err) =>
    alert("Could not open the pairing page: " + err)
  );
});

cancelBtn.addEventListener("click", async () => {
  const state = await pollDialogUi().catch(() => null);
  if (state && state.status === "waiting") {
    invoke("pairing_cancel", { instanceUrl: instanceUrl }).catch(() => {});
  }
  closeDialog();
});

retryBtn.addEventListener("click", startPairing);

async function initPage() {
  const inst = await resolveInstance();
  if (!inst) {
    descEl.textContent = "No servers configured yet. Add one in Settings first.";
    pairedList.textContent = "";
    pairBtn.classList.add("hidden");
    return;
  }
  instanceUrl = inst.url;
  descEl.textContent =
    "Tokens that let this device talk to " +
    inst.name +
    " (" +
    inst.url +
    ") with the identity of the signed-in user. Each pairing is revocable separately on the server.";
  let supported = false;
  try {
    supported = await invoke("pairing_supported", { instanceUrl: instanceUrl });
  } catch {
    supported = false;
  }
  if (!supported) {
    disabledBox.classList.remove("hidden");
    pairBtn.classList.add("hidden");
    pairedList.textContent = "";
    return;
  }
  await renderTokens();
  await resumePairingIfActive();
}

initPage();
