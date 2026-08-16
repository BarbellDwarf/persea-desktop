// Shell UI specs: the local settings/pairing/welcome pages in the Tauri app.
// These exercise the shell's own UI (instances CRUD, appearance, shortcuts,
// pairing states) through the real app binary.
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

async function waitForText(driver, text, timeoutMs = 8000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

module.exports = async function () {
  seedInstances([]);
  const driver = await newSession();

  try {
    // First-run: no instances configured -> the welcome page shows.
    await driver.get(`${SHELL_ORIGIN}/index.html`);
    await waitForText(driver, "Add your first server");
    await screenshot(driver, "shell-welcome");

    // Add an instance through the welcome form. This exercises the
    // ACL-gated instances_add + probe path end to end (regression for
    // "Command instances_add not allowed by ACL").
    const { By } = require("selenium-webdriver");
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        await driver.findElement(By.id("welcome-name")).sendKeys("E2E UI");
        await driver.findElement(By.id("welcome-url")).sendKeys(BASE);
        await driver.findElement(By.id("welcome-form")).submit();
        break;
      } catch (err) {
        if (attempt === 2) throw err;
        await new Promise((r) => setTimeout(r, 1500));
      }
    }
    await new Promise((r) => setTimeout(r, 8000));
    const probeNow = await driver
      .executeScript("return (document.getElementById('welcome-probe')?.innerText || 'NO PROBE EL').slice(0, 500)")
      .catch(() => "EVAL FAILED");
    console.log("DIAG welcome-probe @8s:", JSON.stringify(probeNow));
    try {
      await waitForText(driver, "Server version", 20000);
    } catch (err) {
      const probeText = await driver
        .executeScript("return (document.getElementById('welcome-probe')?.innerText || 'NO PROBE EL').slice(0, 500)")
        .catch(() => "EVAL FAILED");
      console.log("DIAG welcome-probe:", JSON.stringify(probeText));
      throw err;
    }
    await screenshot(driver, "shell-welcome-added");

    // The welcome flow opens the settings page for the guided add.
    // (The add-instance form lives in settings; this spec drives the
    //  form only when the instance store is empty, so it is idempotent.)
    await driver.get(`${SHELL_ORIGIN}/settings.html`);
    await waitForText(driver, "Instances");
    await screenshot(driver, "shell-settings");

    // Pairing page states: without a pairing in flight the page shows
    // the empty/try-again state (no server round trip needed).
    await driver.get(`${SHELL_ORIGIN}/pairing.html?url=${encodeURIComponent(BASE)}`);
    await waitForText(driver, "Pair this device");
    await screenshot(driver, "shell-pairing");

    console.log("shell: welcome, settings, pairing states verified");
  } finally {
    await driver.quit();
  }
};
