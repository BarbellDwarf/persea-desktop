// Post-login navigation audit: drive every main page in the webview after
// the admin login and capture each one. Needs PERSEA_E2E_LOGIN_EMAIL and
// PERSEA_E2E_LOGIN_PASSWORD (skips without them, like login.spec.js).
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
    console.log(
      "navigation: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set",
    );
    return;
  }
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { By } = require("selenium-webdriver");

  try {
    // Admin login, then the dashboard.
    await ensureLoginPage(driver);
    await driver.findElement(By.id("username")).sendKeys(EMAIL);
    await driver.findElement(By.id("password")).sendKeys(PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    await waitForText(driver, "Connections");
    await screenshot(driver, "nav-connections");

    // Every main page after login, with its heading marker.
    const pages = [
      { url: "/sessions.html", text: "Sessions", name: "nav-sessions" },
      { url: "/recordings.html", text: "Recordings", name: "nav-recordings" },
      { url: "/account/profile.html", text: "Profile", name: "nav-profile" },
      { url: "/admin/settings.html", text: "System Settings", name: "nav-admin-settings" },
      { url: "/admin/security.html", text: "Security", name: "nav-admin-security" },
      { url: "/admin/branding.html", text: "Branding", name: "nav-admin-branding" },
      { url: "/admin/reports.html", text: "Reports", name: "nav-admin-reports" },
      { url: "/admin/tunnels.html", text: "SSH Tunnels", name: "nav-admin-tunnels" },
      { url: "/docs", text: "Overview", name: "nav-docs" },
    ];
    for (const page of pages) {
      await driver.get(`${BASE}${page.url}`);
      await waitForText(driver, page.text);
      await screenshot(driver, page.name);
    }

    console.log(`navigation: ${pages.length} pages verified`);
  } finally {
    await driver.quit();
  }
};
