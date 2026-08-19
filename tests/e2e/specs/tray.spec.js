// Tray presence: the app starts with the tray feature enabled. The
// tray is a native menu (tray.rs) and its state-dot icons are native
// window-manager state; WebDriver cannot drive the menu or observe the
// icons, and the shell exposes no tray state through the webview. The
// drivable surface is the app itself: it must start with the tray
// setup running (a tray build failure logs and continues, so a crash
// would be the observable failure) and the shell must stay responsive.
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
    // The app launches with the tray setup (tray.rs setup runs in the
    // startup hook); the shell page rendering is the observable signal
    // that startup completed without a crash.
    await driver.get(`${SHELL_ORIGIN}/index.html`);
    await waitForText(driver, "Add your first server");
    await screenshot(driver, "tray-app-started");

    // The shell stays responsive (the tray poller and event loop run
    // alongside the webview).
    const version = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_app_version').catch(() => null)",
    );
    if (typeof version !== "string" || !version) {
      throw new Error(`the shell did not answer cmd_app_version: ${JSON.stringify(version)}`);
    }

    console.log("tray: app starts with the tray feature enabled (native menu not drivable by WebDriver)");
  } finally {
    await driver.quit();
  }
};
