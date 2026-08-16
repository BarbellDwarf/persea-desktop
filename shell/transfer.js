/* Persea Desktop transfer window.
 *
 * Lists active and finished file transfers with per-file status,
 * progress, retry for failed uploads, open-folder for finished
 * downloads and "Save as" (REST re-download with a save dialog) for
 * drive REST download rows. The Rust side owns the registry; this page
 * renders `transfers_list` and re-renders on `transfers-changed`.
 */

(function () {
  "use strict";

  const LIST = document.getElementById("transfers");
  const EMPTY = document.getElementById("empty");
  const SUMMARY = document.getElementById("summary");
  const CLEAR = document.getElementById("clear-finished");

  const state = { transfers: [] };

  function invoke(cmd, args) {
    const tauri = window.__TAURI_INTERNALS__;
    if (tauri && typeof tauri.invoke === "function") {
      return tauri.invoke(cmd, args || {});
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

  function formatSize(bytes) {
    if (!bytes) return "0 B";
    const units = ["B", "KiB", "MiB", "GiB"];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return (unit === 0 ? value : value.toFixed(1)) + " " + units[unit];
  }

  function statusMeta(t) {
    switch (t.status) {
      case "queued":
        return { text: "Queued", cls: "queued", indeterminate: false };
      case "reading":
        return {
          text: "Reading " + (t.bytesTotal ? percentText(t) : ""),
          cls: "active",
          indeterminate: false,
        };
      case "uploading":
        return { text: "Uploading " + formatSize(t.bytesTotal), cls: "active", indeterminate: true };
      case "downloading":
        return { text: "Downloading", cls: "active", indeterminate: true };
      case "done":
        return {
          text: "Done" + (t.bytesTotal ? " \u00B7 " + formatSize(t.bytesTotal) : ""),
          cls: "done",
          indeterminate: false,
        };
      case "failed":
        return { text: "Failed", cls: "failed", indeterminate: false };
      case "cancelled":
        return { text: "Cancelled", cls: "cancelled", indeterminate: false };
      default:
        return { text: t.status, cls: "", indeterminate: false };
    }
  }

  function percentText(t) {
    if (!t.bytesTotal) return "";
    const pct = Math.min(100, Math.round((100 * t.bytesDone) / t.bytesTotal));
    return pct + "%";
  }

  function progressBar(t) {
    const meta = statusMeta(t);
    const bar = document.createElement("div");
    bar.className = "bar " + meta.cls + (meta.indeterminate ? " indeterminate" : "");
    const fill = document.createElement("div");
    fill.className = "fill";
    if (!meta.indeterminate && t.bytesTotal) {
      const pct = Math.min(100, Math.round((100 * t.bytesDone) / t.bytesTotal));
      fill.style.width = pct + "%";
    } else if (meta.indeterminate) {
      fill.className = "fill indeterminate-fill";
    } else if (t.status === "done") {
      fill.style.width = "100%";
    }
    bar.appendChild(fill);
    return bar;
  }

  function renderRow(t) {
    const li = document.createElement("li");
    li.className = "transfer-row";

    const icon = document.createElement("span");
    icon.className = "direction " + (t.direction === "upload" ? "up" : "down");
    icon.textContent = t.direction === "upload" ? "\u2191" : "\u2193";
    icon.title = t.direction === "upload" ? "Upload" : "Download";
    li.appendChild(icon);

    const body = document.createElement("div");
    body.className = "row-body";

    const nameLine = document.createElement("div");
    nameLine.className = "name-line";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = t.remoteName || t.localName || "file";
    name.title = t.remoteName || t.localName || "";
    nameLine.appendChild(name);
    const meta = statusMeta(t);
    const status = document.createElement("span");
    status.className = "status " + meta.cls;
    status.textContent = meta.text;
    nameLine.appendChild(status);
    body.appendChild(nameLine);

    if (t.localName && t.localName !== t.remoteName) {
      const local = document.createElement("div");
      local.className = "local";
      local.textContent = t.direction === "upload" ? "from " + t.localName : "to " + t.localName;
      body.appendChild(local);
    }

    if (t.error) {
      const error = document.createElement("div");
      error.className = "error";
      error.textContent = t.error;
      error.title = t.error;
      body.appendChild(error);
    }

    body.appendChild(progressBar(t));

    const actions = document.createElement("div");
    actions.className = "actions";
    if (t.canRetry) {
      actions.appendChild(
        actionButton("Retry", "Retry " + (t.remoteName || ""), () => {
          invoke("cmd_transfer_retry", { id: t.id }).catch(() => {});
        })
      );
    }
    if (t.canSaveAs) {
      actions.appendChild(
        actionButton("Save as", "Download " + (t.remoteName || "") + " with a save dialog", () => {
          invoke("cmd_transfer_download", { url: t.sourceUrl }).catch(() => {});
        })
      );
    }
    if (t.canOpenFolder) {
      actions.appendChild(
        actionButton("Open folder", "Show the file in the file manager", () => {
          invoke("cmd_transfer_open_folder", { id: t.id }).catch(() => {});
        })
      );
    }
    if (actions.childElementCount > 0) {
      body.appendChild(actions);
    }

    li.appendChild(body);
    return li;
  }

  function actionButton(label, title, onClick) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn tiny";
    btn.textContent = label;
    btn.title = title;
    btn.addEventListener("click", onClick);
    return btn;
  }

  function render() {
    const transfers = state.transfers;
    const finished = transfers.filter(
      (t) => t.status === "done" || t.status === "failed" || t.status === "cancelled"
    );
    EMPTY.classList.toggle("hidden", transfers.length > 0);
    CLEAR.classList.toggle("hidden", finished.length === 0);
    const done = transfers.filter((t) => t.status === "done").length;
    const failed = transfers.filter((t) => t.status === "failed").length;
    const active = transfers.length - done - failed;
    if (transfers.length > 0) {
      const parts = [];
      if (active > 0) parts.push(active + " active");
      if (done > 0) parts.push(done + " done");
      if (failed > 0) parts.push(failed + " failed");
      SUMMARY.classList.remove("hidden");
      SUMMARY.textContent = parts.join(", ");
    } else {
      SUMMARY.classList.add("hidden");
    }
    LIST.textContent = "";
    transfers.forEach((t) => LIST.appendChild(renderRow(t)));
  }

  CLEAR.addEventListener("click", () => {
    invoke("cmd_transfer_clear_finished")
      .then(onTransfersChanged)
      .catch(() => {});
  });

  function onTransfersChanged(payload) {
    state.transfers = Array.isArray(payload) ? payload : [];
    render();
  }

  invoke("cmd_transfers_list")
    .then(onTransfersChanged)
    .catch(() => {});

  try {
    if (window.__TAURI__ && window.__TAURI__.event) {
      window.__TAURI__.event
        .listen("transfers-changed", (event) => {
          onTransfersChanged(event && event.payload !== undefined ? event.payload : event);
        })
        .catch(() => {});
    }
  } catch (e) { /* IPC unavailable: static empty list */ }
})();
