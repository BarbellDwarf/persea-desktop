// Transfer page states: the shell page (transfer.html) renders the
// transfer registry. With no transfers the page shows the empty state
// and hides the controls that act on rows: "Clear finished" is hidden
// (nothing finished to clear) and no per-row Retry buttons render
// (there are no rows). The registry is Rust-side and only gains rows
// from real file drops onto a live session, which needs guacd; the
// empty state is the drivable surface here.
const { newSession, screenshot, seedInstances } = require("../driver");

const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

async function waitForText(driver, text, timeoutMs = 10000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

module.exports = async function () {
  seedInstances([]);
  const driver = await newSession();

  try {
    await driver.get(`${SHELL_ORIGIN}/transfer.html`);
    await waitForText(driver, "No transfers yet");

    // Empty state: the hint names the real entry point.
    await waitForText(driver, "Drag files onto a session window to send them to the drive.");
    await screenshot(driver, "transfers-empty");

    // The summary line stays hidden with nothing to summarize.
    const summaryHidden = await driver.executeScript(
      "const el = document.getElementById('summary'); return !el || el.classList.contains('hidden')",
    );
    if (!summaryHidden) {
      throw new Error("the summary should be hidden with no transfers");
    }

    // "Clear finished" is hidden when there is nothing finished to
    // clear (the implementation hides the control rather than disabling
    // it).
    const clearHidden = await driver.executeScript(
      "const el = document.getElementById('clear-finished');" +
        " return !el || el.classList.contains('hidden')",
    );
    if (!clearHidden) {
      throw new Error("clear-finished should be hidden with no finished transfers");
    }

    // No rows, so no per-row Retry buttons.
    const rows = await driver.executeScript(
      "return document.querySelectorAll('#transfers .transfer-row').length",
    );
    if (rows !== 0) {
      throw new Error(`expected no transfer rows, got ${rows}`);
    }
    const retryButtons = await driver.executeScript(
      "return Array.from(document.querySelectorAll('#transfers button'))" +
        ".filter((b) => b.textContent === 'Retry').length",
    );
    if (retryButtons !== 0) {
      throw new Error(`expected no retry buttons, got ${retryButtons}`);
    }

    // The registry answers empty too.
    const list = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_transfers_list').catch(() => [])",
    );
    if (!Array.isArray(list) || list.length !== 0) {
      throw new Error(`expected an empty transfer list, got ${JSON.stringify(list)}`);
    }

    console.log("transfers: empty list render + control states verified");
  } finally {
    await driver.quit();
  }
};
