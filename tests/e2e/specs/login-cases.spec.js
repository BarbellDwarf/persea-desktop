// Login-case matrix: different users, emails, and negative cases, driven
// by PERSEA_E2E_LOGIN_CASES (JSON array). Each case runs in its own app
// session and asserts the expected outcome:
//
//   { username, password, expect: "dashboard" | "error", name }
//
// dashboard: the post-login page renders ("Connections" marker).
// error: the login page stays with the server error, no session created.
//
// The matrix skips with a named reason without PERSEA_E2E_LOGIN_CASES.
// Credentials come from the environment; the local and CI matrices use
// the server test fixtures (admin/demo database users, alice/bob LDAP).
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const LOGIN_CASES = process.env.PERSEA_E2E_LOGIN_CASES;

async function waitForText(driver, text, timeoutMs = 20000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

// The webview cookie store persists across app instances, so an earlier
// case's login can land on the dashboard. Log out through the header
// button (POST with CSRF) and wait for the login page.
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
  if (!LOGIN_CASES) {
    console.log("login-cases: skipped, PERSEA_E2E_LOGIN_CASES is not set");
    return;
  }
  let cases;
  try {
    cases = JSON.parse(LOGIN_CASES);
  } catch (err) {
    throw new Error(`PERSEA_E2E_LOGIN_CASES is not valid JSON: ${err.message}`);
  }
  if (!Array.isArray(cases) || cases.length === 0) {
    throw new Error("PERSEA_E2E_LOGIN_CASES must be a non-empty JSON array");
  }

  const { By } = require("selenium-webdriver");
  let failed = 0;
  for (const c of cases) {
    seedInstances([{ name: "Local", url: BASE, default: true }]);
    const driver = await newSession();
    try {
      await ensureLoginPage(driver);
      await driver.findElement(By.id("username")).sendKeys(c.username);
      await driver.findElement(By.id("password")).sendKeys(c.password);
      await driver.findElement(By.id("login-form")).submit();
      if (c.expect === "dashboard") {
        await waitForText(driver, "Connections");
      } else {
        // The server keeps the login page and shows its error verdict.
        await waitForText(driver, "Invalid email or password", 15000);
      }
      await screenshot(driver, `login-case-${c.name}`);
      console.log(`login-case ${c.name}: ${c.expect} PASS`);
    } catch (err) {
      failed += 1;
      console.error(`login-case ${c.name}: ${c.expect} FAIL: ${err.message}`);
    } finally {
      await driver.quit();
    }
  }
  if (failed > 0) {
    throw new Error(`${failed} login case(s) failed`);
  }
  console.log(`login-cases: ${cases.length} cases verified`);
};
