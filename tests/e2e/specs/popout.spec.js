// Session pop-out and pop-in: moving a session tab into its own native
// window and back. A session tab only exists after a live session
// opens, which needs guacd (port 4822); the local/container audit does
// not provide it. The drivable surface is the tab manager commands
// from a shell page: the list answers empty, pop-out and pop-in on a
// missing tab no-op without crashing, and the context-menu command
// errors cleanly with "no tab <id>".
const { newSession, screenshot, seedInstances } = require("../driver");

const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

module.exports = async function () {
  seedInstances([]);
  const driver = await newSession();

  try {
    await driver.get(`${SHELL_ORIGIN}/index.html`);
    const { until, By } = require("selenium-webdriver");
    await driver.wait(
      until.elementLocated(By.xpath("//*[contains(text(), 'Add your first server')]")),
      10000,
    );

    // The tab manager answers with the empty list.
    const tabs = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_list').catch((e) => ({ __error: String(e) }))",
    );
    if (!Array.isArray(tabs) || tabs.length !== 0) {
      throw new Error(`expected an empty tab list, got ${JSON.stringify(tabs)}`);
    }

    // Pop-out on a missing tab: the manager no-ops (the command
    // resolves, no window is created, the list stays empty).
    const popOut = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_pop_out', { id: 'missing-tab' })" +
        ".then(() => ({ ok: true })).catch((e) => ({ ok: false, error: String(e) }))",
    );
    if (!popOut || popOut.ok !== true) {
      throw new Error(`cmd_tabs_pop_out on a missing tab should no-op, got ${JSON.stringify(popOut)}`);
    }
    const tabsAfterPopOut = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_list').catch(() => [])",
    );
    if (!Array.isArray(tabsAfterPopOut) || tabsAfterPopOut.length !== 0) {
      throw new Error(`pop-out on a missing tab created a tab: ${JSON.stringify(tabsAfterPopOut)}`);
    }

    // Pop-in on a missing tab: the same no-op.
    const popIn = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_pop_in', { id: 'missing-tab' })" +
        ".then(() => ({ ok: true })).catch((e) => ({ ok: false, error: String(e) }))",
    );
    if (!popIn || popIn.ok !== true) {
      throw new Error(`cmd_tabs_pop_in on a missing tab should no-op, got ${JSON.stringify(popIn)}`);
    }

    // The context-menu command validates the tab id and errors cleanly.
    const ctx = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_context_menu', { id: 'missing-tab', x: 0, y: 0 })" +
        ".then(() => ({ ok: true })).catch((e) => ({ ok: false, error: String(e) }))",
    );
    if (!ctx || ctx.ok !== false || !String(ctx.error).includes("no tab")) {
      throw new Error(
        `cmd_tabs_context_menu on a missing tab should error with 'no tab', got ${JSON.stringify(ctx)}`,
      );
    }

    await screenshot(driver, "popout-empty-manager");
    console.log("popout: tab manager commands answer cleanly with no session tabs");
  } finally {
    await driver.quit();
  }
};
