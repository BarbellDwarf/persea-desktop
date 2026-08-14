/* Persea Desktop shell chrome (D02).
 *
 * Shared by every shell page (index.html, settings.html). Provides:
 *  - invoke(): Tauri IPC without the npm API layer
 *  - initTheme(): shell theme system (light / dark / auto)
 *  - appVersion(): version from the app binary
 *  - copyText(): shell clipboard helper (plugin, shell pages only)
 *  - external link handling via the opener plugin
 *  - first-run welcome flow (index.html only, guarded by element presence)
 */

const APP_VERSION_FALLBACK = "1.0.0";

function invoke(cmd, args = {}) {
  const tauri = window.__TAURI_INTERNALS__;
  if (tauri && typeof tauri.invoke === "function") {
    return tauri.invoke(cmd, args);
  }
  return Promise.reject(new Error("Tauri IPC is not available"));
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[c]));
}

async function appVersion() {
  try {
    const v = await invoke("cmd_app_version");
    return typeof v === "string" && v ? v : APP_VERSION_FALLBACK;
  } catch {
    return APP_VERSION_FALLBACK;
  }
}

/* Shell clipboard: tauri-plugin-clipboard-manager (write-text, shell
 * pages only; the remote instance never gets these permissions).
 * Falls back to the web clipboard API. Returns true on success. */
async function copyText(text) {
  try {
    await invoke("plugin:clipboard-manager|write_text", { text });
    return true;
  } catch {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }
}

/* Shell theme system: token stylesheet in settings.css. "auto" follows
 * the OS via prefers-color-scheme; explicit light/dark pin a class. */
function initTheme() {
  const media = window.matchMedia("(prefers-color-scheme: light)");
  let setting = "auto";

  const apply = () => {
    const resolved =
      setting === "auto" ? (media.matches ? "light" : "dark") : setting;
    document.documentElement.classList.toggle("light", resolved === "light");
    document.documentElement.classList.toggle("dark", resolved === "dark");
  };

  const onMediaChange = () => {
    if (setting === "auto") apply();
  };
  if (media.addEventListener) {
    media.addEventListener("change", onMediaChange);
  } else {
    media.addListener(onMediaChange);
  }

  apply();
  invoke("cmd_shell_get_settings")
    .then((s) => {
      if (s && s.appearance) {
        setting = s.appearance;
        apply();
      }
    })
    .catch(() => {});
}

function initExternalLinks() {
  document.addEventListener("click", (event) => {
    const anchor = event.target.closest("a[data-external]");
    if (!anchor) return;
    event.preventDefault();
    invoke("plugin:opener|open_url", { url: anchor.href }).catch(() => {
      window.open(anchor.href, "_blank", "noopener,noreferrer");
    });
  });
}

/* ------------------------------------------------------------------ */
/* First-run welcome + app chrome (index.html)                        */
/* ------------------------------------------------------------------ */

function renderProbeSummary(container, inst) {
  const probe = inst.probe;
  const parts = [];
  if (probe && probe.needsSetup) {
    parts.push(
      '<div class="setup-banner">' +
        "<span>This server has not been set up yet.</span>" +
        '<button type="button" class="btn btn-accent" data-open-setup="' +
        escapeHtml(inst.url) +
        '">Open setup</button></div>'
    );
  }
  if (!probe) {
    parts.push('<p class="probe-pending">Server not checked yet.</p>');
  } else if (!probe.ok) {
    const known = probe.version && probe.version !== "unknown";
    parts.push(
      '<p class="probe-error">Unreachable' +
        (known ? " — last known version " + escapeHtml(probe.version) : " — never reached") +
        ".</p>"
    );
  } else {
    parts.push('<p class="probe-pending">Server version ' + escapeHtml(probe.version) + "</p>");
    if (probe.updateAvailable && probe.latestVersion) {
      parts.push(
        '<p class="update-note">Server update available: ' +
          escapeHtml(probe.latestVersion) +
          "</p>"
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
  }
  container.innerHTML = parts.join("");
  container.querySelector("[data-open-setup]")?.addEventListener("click", (e) => {
    e.preventDefault();
    invoke("cmd_instances_open_setup", { url: e.currentTarget.dataset.openSetup }).catch(() => {});
  });
}

function capabilityChips(capabilities) {
  const labels = {
    drive_api: "drive API",
    drive_upload: "drive upload",
    session_events: "session events",
    desktop_pairing: "device pairing",
    desktop_bridge: "desktop bridge",
    kiosk_allowed: "kiosk",
    desktop_transfers: "drag-drop transfers",
  };
  return Object.entries(labels)
    .filter(([key]) => key in capabilities)
    .map(([key, label]) => {
      const on = capabilities[key] === true;
      return (
        '<span class="chip ' +
        (on ? "on" : "off") +
        '" title="' +
        (on ? "enabled on server" : "disabled on server") +
        '">' +
        label +
        (on ? " ✓" : " ✕") +
        "</span>"
      );
    });
}

function wireWelcome() {
  const welcome = document.getElementById("welcome");
  const form = document.getElementById("welcome-form");
  const probe = document.getElementById("welcome-probe");
  const openBtn = document.getElementById("welcome-open");
  if (!welcome || !form) return;

  welcome.classList.remove("hidden");

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const name = form.elements.name.value.trim();
    const url = form.elements.url.value.trim();
    probe.innerHTML = '<p class="probe-pending">Checking server…</p>';
    openBtn.classList.add("hidden");
    invoke("cmd_instances_add", { name, url })
      .then((inst) => {
        renderProbeSummary(probe, inst);
        openBtn.classList.remove("hidden");
        openBtn.addEventListener("click", () => {
          invoke("cmd_instances_open", { url: inst.url }).catch(() => {});
        });
      })
      .catch((err) => {
        probe.innerHTML = '<p class="probe-error">' + escapeHtml(String(err)) + "</p>";
      });
  });
}

async function initWelcomePage() {
  const welcome = document.getElementById("welcome");
  if (!welcome) return; // settings page, nothing to do here
  let instances = [];
  try {
    instances = await invoke("cmd_instances_list");
  } catch {
    instances = [];
  }
  if (instances.length) {
    // Rust auto-opens the default/last instance at startup; this branch
    // covers a manual return to the shell page.
    invoke("cmd_instances_open_default").catch(() => wireWelcome());
  } else {
    wireWelcome();
  }
}

async function initChrome() {
  const versionEl = document.getElementById("app-version");
  if (versionEl) versionEl.textContent = "version " + (await appVersion());

  const settingsBtn = document.getElementById("btn-settings");
  if (settingsBtn) {
    settingsBtn.addEventListener("click", () => {
      window.location.href = "settings.html";
    });
  }
}

initTheme();
initExternalLinks();
initChrome();
initWelcomePage();
