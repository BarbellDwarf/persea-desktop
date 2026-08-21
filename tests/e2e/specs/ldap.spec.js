// LDAP-backed login: with the LDAP provider configured on the server
// (admin auth page; the running chain needs a restart to pick it up),
// log out of the webview and log in with the LDAP test account, then
// verify the dashboard loads. Skips with a named reason when the
// provider is not configured. Credentials match the server test fixtures
// (tests/fixtures/ldap-seed.ldif: alice / alice-ldap-password-2026).
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const ADMIN_EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const ADMIN_PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;
const LDAP_USERNAME = process.env.PERSEA_E2E_LDAP_USERNAME || "alice";
const LDAP_PASSWORD = process.env.PERSEA_E2E_LDAP_PASSWORD || "alice-ldap-password-2026";

async function waitForText(driver, text, timeoutMs = 20000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

// The webview cookie store persists across app instances, so an earlier
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
  if (!ADMIN_EMAIL || !ADMIN_PASSWORD) {
    console.log(
      "ldap: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set",
    );
    return;
  }
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { By, until } = require("selenium-webdriver");

  try {
    // Log in as admin in the webview, then check the provider list via
    // the API with the same session (the spec process talks to the
    // server directly; node fetch keeps the cookie manually).
    await ensureLoginPage(driver);
    await driver.findElement(By.id("username")).sendKeys(ADMIN_EMAIL);
    await driver.findElement(By.id("password")).sendKeys(ADMIN_PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    await waitForText(driver, "Connections");

    const jar = {};
    const absorb = (res) => {
      for (const c of res.headers.getSetCookie ? res.headers.getSetCookie() : []) {
        const [pair] = c.split(";");
        const idx = pair.indexOf("=");
        if (idx > 0) jar[pair.slice(0, idx)] = pair.slice(idx + 1);
      }
      return res;
    };
    const base = BASE.replace(/\/$/, "");
    const cookieHeader = () =>
      Object.entries(jar).map(([k, v]) => `${k}=${v}`).join("; ");
    await absorb(
      await fetch(`${base}/`, { headers: { cookie: cookieHeader() }, redirect: "manual" }),
    );
    await absorb(
      await fetch(`${base}/auth/login`, {
        method: "POST",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
          cookie: cookieHeader(),
        },
        body: new URLSearchParams({
          csrf_token: jar.csrf_token || "",
          username: ADMIN_EMAIL,
          password: ADMIN_PASSWORD,
        }),
        redirect: "manual",
      }),
    );
    const providersRes = await fetch(`${base}/api/auth/providers`, {
      headers: { cookie: cookieHeader() },
    });
    const payload = await providersRes.json();
    const providers = Array.isArray(payload) ? payload : payload.providers || [];
    const ldapProvider = providers.find((p) => p.type === "ldap" && p.enabled);
    if (!ldapProvider) {
      console.log(
        "ldap: skipped, no enabled LDAP provider on the server (configure it via the admin auth page; the running chain needs a restart)",
      );
      return;
    }
    console.log(`ldap: provider '${ldapProvider.name}' present`);

    // Log out through the header button (POST with CSRF), then log in
    // with the LDAP account.
    await driver.wait(until.elementLocated(By.id("logout-btn")), 10000);
    await driver.findElement(By.id("logout-btn")).click();
    await waitForText(driver, "Sign in");
    await driver.findElement(By.id("username")).sendKeys(LDAP_USERNAME);
    await driver.findElement(By.id("password")).sendKeys(LDAP_PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    try {
      await waitForText(driver, "Connections");
    } catch (err) {
      // Surface the server's verdict: a redirect like
      // /?error=user_lookup_failed pinpoints the failure (a server-side
      // LDAP subject bug, persea#235, made this fail historically).
      const url = await driver.getCurrentUrl().catch(() => "unknown");
      const text = await driver
        .executeScript("return (document.body && document.body.innerText || '').slice(0, 300)")
        .catch(() => "");
      throw new Error(`${err.message}; after LDAP login url=${url}; page: ${JSON.stringify(text)}`);
    }
    await screenshot(driver, "ldap-dashboard");

    // Group auto-provisioning: alice is a member of the engineers group
    // in LDAP; the server should have provisioned it locally.
    const groupsRes = await fetch(`${base}/api/admin/groups`, {
      headers: { cookie: cookieHeader() },
    });
    const groups = await groupsRes.json();
    const hasEngineers = (Array.isArray(groups) ? groups : groups.groups || []).some(
      (g) => g.name === "engineers" || g.name === "Engineers",
    );
    if (!hasEngineers) {
      throw new Error(`engineers group not provisioned after the LDAP login; groups: ${JSON.stringify(groups).slice(0, 200)}`);
    }
    console.log("ldap: engineers group auto-provisioned");

    console.log("ldap: LDAP-backed login verified");
  } finally {
    await driver.quit();
  }
};
