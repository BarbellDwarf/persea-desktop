// Page-render specs: every shell page loads with its expected structure in
// the real app. Catches blank pages, broken script loads and missing
// sections without needing a server round trip.
const { newSession, screenshot, seedInstances } = require("../driver");

const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

async function waitForText(driver, text, timeoutMs = 10000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

module.exports = async function () {
  seedInstances([]);
  const driver = await newSession();
  const { until, By } = require("selenium-webdriver");

  try {
    const pages = [
      { url: "/index.html", text: "Add your first server", name: "welcome" },
      { url: "/settings.html", text: "Instances", name: "settings" },
      { url: "/pairing.html", text: "Device pairing", name: "pairing" },
      { url: "/transfer.html", text: "Transfers", name: "transfer" },
      { url: "/dropzone.html", text: "Drop to send files", name: "dropzone" },
      { url: "/login.html", text: "Log in", name: "login" },
    ];
    for (const page of pages) {
      await driver.get(`${SHELL_ORIGIN}${page.url}`);
      await waitForText(driver, page.text);
      await screenshot(driver, `pages-${page.name}`);
    }

    // Tabstrip: its "Active sessions" marker is an aria-label, not text,
    // so assert the strip chrome element instead.
    await driver.get(`${SHELL_ORIGIN}/tabstrip.html`);
    await driver.wait(until.elementLocated(By.id("strip")), 10000);
    await driver.wait(until.elementLocated(By.id("tabs")), 10000);
    await screenshot(driver, "pages-tabstrip");

    // Settings completeness: every v1.1.0 section must be present.
    await driver.get(`${SHELL_ORIGIN}/settings.html`);
    const sections = [
      "sec-instances",
      "sec-appearance",
      "sec-performance",
      "sec-network",
      "sec-shortcuts",
      "sec-notifications",
      "sec-updates",
      "sec-kiosk",
      "sec-about",
    ];
    for (const id of sections) {
      await driver.wait(until.elementLocated(By.id(id)), 10000);
    }
    console.log("pages: all shell pages render with expected markers and sections");
  } finally {
    await driver.quit();
  }
};
