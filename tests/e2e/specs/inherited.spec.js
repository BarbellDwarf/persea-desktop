// Inherited web-feature specs: the REMOTE persea UI inside the app's
// webview. Covers the high-risk engine behaviors (login, page render,
// session open flow). Session-open and drive specs need guacd and are
// exercised in the full CI matrix; they degrade to render checks here.
const { newSession, screenshot } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;

async function waitForText(driver, text, timeoutMs = 10000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

module.exports = async function () {
  const driver = await newSession();

  try {
    // Login page renders (the persea instance webview).
    await driver.get(BASE);
    await waitForText(driver, "Sign in");
    await screenshot(driver, "web-login");

    // Docs page is public.
    await driver.get(`${BASE}/docs`);
    await waitForText(driver, "persea");

    // Password login with the e2e admin, then the connections page.
    const { By } = require("selenium-webdriver");
    const adminPass = process.env.PERSEA_E2E_ADMIN_PASSWORD || "e2e-admin-password-12345";
    await driver.findElement(By.name("username")).sendKeys("e2e-admin");
    await driver.findElement(By.name("password")).sendKeys(adminPass);
    await driver.findElement(By.css("form")).submit();
    await waitForText(driver, "Connections");
    await screenshot(driver, "web-connections");

    // Sessions page renders and polls.
    await driver.get(`${BASE}/sessions.html`);
    await waitForText(driver, "Sessions");

    // Admin settings render (incl. the Desktop toggles section).
    await driver.get(`${BASE}/admin/settings.html`);
    await waitForText(driver, "Desktop");

    console.log("inherited: login, connections, sessions, admin settings verified");
  } finally {
    await driver.quit();
  }
};
