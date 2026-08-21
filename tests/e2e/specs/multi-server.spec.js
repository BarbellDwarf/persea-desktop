// Multi-server: two configured servers with independent per-server
// logins, and switching between them preserves each server's session
// (the per-instance webview stores keep the cookies separate). Driven by
// PERSEA_E2E_SERVERS (JSON array of
// { name, url, email, password }); the first server is the default.
// Skips without it.
//
// The switch-back persistence cases only run with real per-instance
// stores: under the WebDriver automation the app is forced onto the
// shared webview store (the driver depends on it), so the collision
// persists there regardless of the app's store isolation. Set
// PERSEA_E2E_REAL_STORES=1 when the app runs with real per-instance
// stores (manual runs); otherwise those cases skip with this reason.
const { newSession, screenshot, seedInstances } = require("../driver");

const SERVERS = process.env.PERSEA_E2E_SERVERS;

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
  if (!SERVERS) {
    console.log("multi-server: skipped, PERSEA_E2E_SERVERS is not set");
    return;
  }
  let servers;
  try {
    servers = JSON.parse(SERVERS);
  } catch (err) {
    throw new Error(`PERSEA_E2E_SERVERS is not valid JSON: ${err.message}`);
  }
  if (!Array.isArray(servers) || servers.length < 2) {
    throw new Error("PERSEA_E2E_SERVERS needs at least two servers");
  }

  seedInstances(servers.map((s, i) => ({ name: s.name, url: s.url, default: i === 0 })));
  const driver = await newSession();
  const { By } = require("selenium-webdriver");

  let failed = 0;
  const check = async (label, fn) => {
    try {
      await fn();
      console.log(`multi-server ${label}: PASS`);
    } catch (err) {
      failed += 1;
      console.error(`multi-server ${label} FAIL: ${err.message}`);
    }
  };

  // The profile page fills #email from the API; the identity check reads
  // the input value rather than page text.
  const checkIdentity = async (server) => {
    await driver.get(`${server.url}/account/profile.html`);
    await driver.wait(async () => {
      const el = await driver.findElement(By.css("#email")).catch(() => null);
      if (!el) return false;
      return (await el.getAttribute("value")) === server.email;
    }, 20000);
    await screenshot(driver, `multi-server-profile-${server.name.replace(/\s+/g, "-").toLowerCase()}`);
  };

  try {
    // Server 1 (default): the app auto-opens it; log in and verify the
    // identity on the profile page.
    await check("server1 login", async () => {
      await ensureLoginPage(driver);
      await driver.findElement(By.id("username")).sendKeys(servers[0].email);
      await driver.findElement(By.id("password")).sendKeys(servers[0].password);
      await driver.findElement(By.id("login-form")).submit();
      await waitForText(driver, "Connections");
      await screenshot(driver, "multi-server-1-logged-in");
    });
    await check("server1 identity", () => checkIdentity(servers[0]));

    // Switch to server 2: its own store has no cookie, so the login page
    // shows; log in with server 2's account.
    await check("server2 login", async () => {
      await driver.get(`${servers[1].url}/`);
      await ensureLoginPage(driver);
      await driver.findElement(By.id("username")).sendKeys(servers[1].email);
      await driver.findElement(By.id("password")).sendKeys(servers[1].password);
      await driver.findElement(By.id("login-form")).submit();
      await waitForText(driver, "Connections");
      await screenshot(driver, "multi-server-2-logged-in");
    });
    await check("server2 identity", () => checkIdentity(servers[1]));

    // Switch back to server 1: no re-login, its session persisted in its
    // own store. Only verifiable with real per-instance stores.
    const realStores = process.env.PERSEA_E2E_REAL_STORES === "1";
    if (realStores) {
      await check("server1 switch back", () => checkIdentity(servers[0]));
      await check("server2 switch back", () => checkIdentity(servers[1]));
    } else {
      console.log(
        "multi-server: switch-back persistence skipped (the WebDriver automation forces the shared webview store; per-instance switching is covered by the rebuild unit tests)",
      );
    }
  } finally {
    await driver.quit();
  }

  if (failed > 0) {
    throw new Error(`${failed} multi-server case(s) failed`);
  }
  console.log(`multi-server: ${servers.length} servers, switching verified`);
};
