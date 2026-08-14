/* Persea Desktop tab strip (window manager, shell-owned).
 *
 * Rendered inside the dedicated `tabstrip` window, docked above the
 * main window. Mirrors the web client's session tab language: status
 * dot, protocol badge, hostname label. The Rust side owns the tab
 * state; this page renders `tabs_list` and re-renders on
 * `tabs-changed`.
 *
 * Interaction:
 *  - click tab: activate (switch / restore / focus per mode)
 *  - × button: close the tab view
 *  - right-click: native context menu (pop out/in, expand to monitor,
 *    copy share link, terminate) via tabs_context_menu
 *  - Ctrl+K: toggle the session list popover (shell tab switching)
 *  - Ctrl+Tab / Ctrl+Shift+Tab: cycle tabs
 *  - Arrow keys cycle tab focus, Enter/Space activates, Esc closes menus
 */

(function () {
  "use strict";

  const TAB_ROW = document.getElementById("tabs");
  const OVERFLOW_BTN = document.getElementById("overflow-btn");
  const OVERFLOW = document.getElementById("overflow");

  const state = { tabs: [], focusIndex: -1 };

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

  function statusMeta(tab) {
    if (tab.status === "live") return { cls: "dot-live", label: "live" };
    if (tab.status === "ended") return { cls: "dot-ended", label: "ended" };
    return { cls: "dot-connecting", label: "connecting" };
  }

  function modeLabel(tab) {
    if (tab.mode === "popped") return ", in its own window";
    if (tab.mode === "expanded") {
      return ", fullscreen on " + (tab.monitor ? tab.monitor : "a display");
    }
    return "";
  }

  function badgeText(tab) {
    return (tab.protocol || "SES").toUpperCase();
  }

  function tabAriaLabel(tab) {
    const dot = statusMeta(tab);
    return (
      (tab.title || "Session " + tab.id) +
      ", " + dot.label +
      modeLabel(tab) +
      (tab.active ? ", active" : "")
    );
  }

  function renderTab(tab, index) {
    const el = document.createElement("div");
    el.className = "tab" + (tab.active ? " active" : "") + (tab.status === "ended" ? " ended" : "");
    el.setAttribute("role", "tab");
    el.setAttribute("aria-selected", String(tab.active));
    el.setAttribute("aria-label", tabAriaLabel(tab));
    el.dataset.id = tab.id;
    el.dataset.index = String(index);
    el.tabIndex = -1;

    const dot = document.createElement("span");
    dot.className = "dot " + statusMeta(tab).cls;
    el.appendChild(dot);

    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = badgeText(tab);
    badge.title = tab.protocol ? tab.protocol : "session";
    el.appendChild(badge);

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = tab.title || "Session " + tab.id;
    el.appendChild(name);

    if (tab.mode === "popped" || tab.mode === "expanded") {
      const icon = document.createElement("span");
      icon.className = "mode-icon";
      icon.textContent = tab.mode === "expanded" ? "\u26F6" : "\u2197";
      icon.title = tab.mode === "expanded"
        ? "Session on " + (tab.monitor ? tab.monitor : "a display")
        : "Session in its own window";
      el.appendChild(icon);
    }

    const close = document.createElement("button");
    close.type = "button";
    close.className = "close";
    close.textContent = "\u00D7";
    close.title = "Close tab";
    close.setAttribute("aria-label", "Close tab " + (tab.title || tab.id));
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      invoke("cmd_tabs_close", { id: tab.id }).catch(() => {});
    });
    el.appendChild(close);
    return el;
  }

  function render() {
    const tabs = state.tabs;
    // Active tab first so overflow never hides it (the rest keep
    // insertion order).
    const ordered = tabs
      .filter((t) => t.active)
      .concat(tabs.filter((t) => !t.active));
    TAB_ROW.textContent = "";
    ordered.forEach((tab, index) => TAB_ROW.appendChild(renderTab(tab, index)));
    state.focusIndex = Math.min(Math.max(state.focusIndex, 0), Math.max(tabs.length - 1, 0));
    if (tabs.length > 0) {
      const activeEl = TAB_ROW.querySelector(".tab.active") || TAB_ROW.firstElementChild;
      activeEl.tabIndex = 0;
    }
    applyOverflow();
  }

  function applyOverflow() {
    const tabs = Array.from(TAB_ROW.querySelectorAll(".tab"));
    // The overflow button lives outside the tabs row, so only its own
    // width plus spacing eats into the row's room.
    const reserve = OVERFLOW_BTN.offsetWidth + 12;
    const maxRight = TAB_ROW.clientWidth - reserve;
    let overflowed = [];
    // Remove tabs from the END (never the active, which is first) until
    // everything fits.
    for (let i = tabs.length - 1; i >= 0; i--) {
      const el = tabs[i];
      const right = el.offsetLeft + el.offsetWidth;
      if (right > maxRight && el.offsetLeft > 0) {
        overflowed.unshift(el.dataset.id);
        el.classList.add("hidden-tab");
      }
    }
    const anyOverflow = overflowed.length > 0;
    OVERFLOW_BTN.classList.toggle("hidden", !anyOverflow);
    if (anyOverflow) {
      OVERFLOW.textContent = "";
      state.tabs
        .filter((t) => overflowed.includes(t.id))
        .forEach((tab) => {
          const item = document.createElement("button");
          item.type = "button";
          item.className = "overflow-item" + (tab.active ? " active" : "");
          item.setAttribute("role", "menuitem");
          item.textContent = (tab.title || "Session " + tab.id) + modeLabel(tab);
          item.setAttribute("aria-label", tabAriaLabel(tab));
          item.addEventListener("click", () => activate(tab.id));
          OVERFLOW.appendChild(item);
        });
      if (!OVERFLOW.classList.contains("hidden")) {
        // Re-measure while open: the window follows the list height.
        const height = Math.min(OVERFLOW.scrollHeight || 120, 320);
        invoke("cmd_tabs_overflow", { open: true, height }).catch(() => {});
      }
    } else {
      OVERFLOW.textContent = "";
      if (!OVERFLOW.classList.contains("hidden")) {
        closeOverflowList();
      }
    }
  }

  function activate(id) {
    invoke("cmd_tabs_switch", { id }).catch(() => {});
  }

  function focusTab(index) {
    const els = TAB_ROW.querySelectorAll(".tab");
    if (els.length === 0) return;
    const clamped = (index + els.length) % els.length;
    els.forEach((el) => (el.tabIndex = -1));
    els[clamped].tabIndex = 0;
    els[clamped].focus();
    state.focusIndex = clamped;
  }

  function toggleOverflowList() {
    if (OVERFLOW.classList.contains("hidden")) {
      OVERFLOW.classList.remove("hidden");
      OVERFLOW_BTN.setAttribute("aria-expanded", "true");
      // Grow the strip window so the popover is visible below the
      // 44 px tab row.
      const height = Math.min(OVERFLOW.scrollHeight || 120, 320);
      invoke("cmd_tabs_overflow", { open: true, height }).catch(() => {});
      OVERFLOW.firstElementChild && OVERFLOW.firstElementChild.focus();
    } else {
      closeOverflowList();
    }
  }

  function closeOverflowList() {
    if (!OVERFLOW.classList.contains("hidden")) {
      OVERFLOW.classList.add("hidden");
      OVERFLOW_BTN.setAttribute("aria-expanded", "false");
      invoke("cmd_tabs_overflow", { open: false }).catch(() => {});
    }
  }

  function openContextMenu(tab, event) {
    event.preventDefault();
    event.stopPropagation();
    // Strip-relative cursor position (logical px); the Rust side shows
    // the native menu there.
    invoke("cmd_tabs_context_menu", { id: tab.id, x: event.clientX, y: event.clientY }).catch(() => {});
  }

  /* ---------------------------------------------------------------- */
  /* Event wiring                                                      */
  /* ---------------------------------------------------------------- */

  TAB_ROW.addEventListener("click", (event) => {
    const tabEl = event.target.closest(".tab");
    if (!tabEl) return;
    activate(tabEl.dataset.id);
  });

  TAB_ROW.addEventListener("contextmenu", (event) => {
    const tabEl = event.target.closest(".tab");
    if (!tabEl) return;
    const tab = state.tabs.find((t) => t.id === tabEl.dataset.id);
    if (tab) openContextMenu(tab, event);
  });

  TAB_ROW.addEventListener("keydown", (event) => {
    const els = Array.from(TAB_ROW.querySelectorAll(".tab"));
    if (els.length === 0) return;
    let handled = true;
    if (event.key === "ArrowRight") {
      focusTab(state.focusIndex + 1);
    } else if (event.key === "ArrowLeft") {
      focusTab(state.focusIndex - 1);
    } else if (event.key === "Home") {
      focusTab(0);
    } else if (event.key === "End") {
      focusTab(els.length - 1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const el = document.activeElement;
      if (el && el.classList.contains("tab")) activate(el.dataset.id);
    } else {
      handled = false;
    }
    if (handled) event.preventDefault();
  });

  OVERFLOW_BTN.addEventListener("click", toggleOverflowList);

  document.addEventListener("keydown", (event) => {
    const mod = event.ctrlKey || event.metaKey;
    if (mod && !event.altKey && event.key === "k") {
      event.preventDefault();
      toggleOverflowList();
      return;
    }
    if (mod && event.key === "Tab") {
      event.preventDefault();
      if (event.shiftKey) invoke("cmd_tabs_prev").catch(() => {});
      else invoke("cmd_tabs_next").catch(() => {});
      return;
    }
    if (event.key === "Escape") {
      closeOverflowList();
      document.activeElement && document.activeElement.blur();
    }
  });

  OVERFLOW.addEventListener("click", (event) => {
    if (!event.target.closest(".overflow-item")) closeOverflowList();
  });

  window.addEventListener("blur", () => {
    closeOverflowList();
  });

  /* ---------------------------------------------------------------- */
  /* IPC: initial list + live updates                                  */
  /* ---------------------------------------------------------------- */

  function onTabsChanged(payload) {
    state.tabs = Array.isArray(payload) ? payload : [];
    render();
  }

  invoke("cmd_tabs_list")
    .then(onTabsChanged)
    .catch(() => {});

  try {
    if (window.__TAURI__ && window.__TAURI__.event) {
      window.__TAURI__.event
        .listen("tabs-changed", (event) => {
          onTabsChanged(event && event.payload !== undefined ? event.payload : event);
        })
        .catch(() => {});
    }
  } catch (e) { /* IPC unavailable: static empty strip */ }
})();
