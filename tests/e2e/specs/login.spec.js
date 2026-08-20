// Full login flow: renders the login page, submits real credentials,
// and verifies the dashboard loads. Credentials come from env
// (PERSEA_E2E_LOGIN_EMAIL / PERSEA_E2E_LOGIN_PASSWORD); without them
// the spec skips with a named reason, because the provisioned server is
// used for state checks, not credential flows.
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;

async function waitForText(driver, text, timeoutMs = 20000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

// The webview cookie store persists across app instances, so a previous
// spec's login can land here on the dashboard. Log out through the
// header button (POST with CSRF) and wait for the login page.
async function ensureLoginPage(driver) {
  const { until, By } = require("selenium-webdriver");
  try {
    await waitForText(driver, "Sign in", 8000);
  } catch {
    await driver.wait(until.elementLocated(By.id("logout-btn")), 10000);
    await driver.findElement(By.id("logout-btn")).click();
    await waitForText(driver, "Sign in", 15000);
  }
}

module.exports = async function () {
  if (!EMAIL || !PASSWORD) {
    console.log("login: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set");
    return;
  }
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { By } = require("selenium-webdriver");

  try {
    // The login page renders in the viewport (the auto-open target).
    await ensureLoginPage(driver);
    await screenshot(driver, "login-page");

    // Submit the real credentials and verify the dashboard loads.
    await driver.findElement(By.id("username")).sendKeys(EMAIL);
    await driver.findElement(By.id("password")).sendKeys(PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    await waitForText(driver, "Connections");
    await screenshot(driver, "login-dashboard");

    console.log("login: full credential flow verified");
  } finally {
    await driver.quit();
  }
};
