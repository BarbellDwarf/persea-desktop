// Live SSH connection: create an SSH entry through the server API against
// a local SSH target, open it in the webview, and verify the session
// renders (the client page's #status element flips to connected). The
// target and guacd must be reachable from the server. Needs
// PERSEA_E2E_LOGIN_EMAIL/PASSWORD; the entry is created fresh per run.
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;
// The local SSH target. The docker bridge gateway (172.17.0.1) reaches
// the host's published port from a server container.
const TARGET_HOST = process.env.PERSEA_E2E_SSH_HOST || "172.17.0.1";
const TARGET_PORT = Number(process.env.PERSEA_E2E_SSH_PORT || 2222);
const TARGET_USER = process.env.PERSEA_E2E_SSH_USER || "sshuser";
const TARGET_PASSWORD = process.env.PERSEA_E2E_SSH_PASSWORD || "ssh-test-password-2026";

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

// Log in over the API and return { jar, get, post } with the session
// cookie absorbed. node 18 fetch has no cookie jar, so the jar is manual.
function entrySlug(name) {
  return name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

async function apiLogin(baseUrl, email, password) {
  const jar = {};
  const cookieHeader = () =>
    Object.entries(jar).map(([k, v]) => `${k}=${v}`).join("; ");
  const absorb = (res) => {
    for (const c of res.headers.getSetCookie ? res.headers.getSetCookie() : []) {
      const [pair] = c.split(";");
      const idx = pair.indexOf("=");
      if (idx > 0) jar[pair.slice(0, idx)] = pair.slice(idx + 1);
    }
    return res;
  };
  const base = baseUrl.replace(/\/$/, "");

  await absorb(
    await fetch(`${base}/`, { headers: { cookie: cookieHeader() }, redirect: "manual" }),
  );
  const login = await absorb(
    await fetch(`${base}/auth/login`, {
      method: "POST",
      headers: {
        "content-type": "application/x-www-form-urlencoded",
        cookie: cookieHeader(),
      },
      body: new URLSearchParams({
        csrf_token: jar.csrf_token || "",
        username: email,
        password,
      }),
      redirect: "manual",
    }),
  );
  if (![302, 303].includes(login.status)) {
    throw new Error(`API login failed with HTTP ${login.status}`);
  }
  return {
    jar,
    async get(path) {
      return absorb(
        await fetch(`${base}${path}`, { headers: { cookie: cookieHeader() }, redirect: "manual" }),
      );
    },
    async post(path, body) {
      return absorb(
        await fetch(`${base}${path}`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            cookie: cookieHeader(),
            "x-csrf-token": jar.csrf_token || "",
          },
          body: JSON.stringify(body),
        }),
      );
    },
    async del(path) {
      return absorb(
        await fetch(`${base}${path}`, {
          method: "DELETE",
          headers: { cookie: cookieHeader(), "x-csrf-token": jar.csrf_token || "" },
        }),
      );
    },
  };
}

module.exports = async function () {
  if (!EMAIL || !PASSWORD) {
    console.log(
      "connection: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set",
    );
    return;
  }
  // One or more SSH targets. PERSEA_E2E_SSH_TARGETS (JSON array of
  // { host, port, user, password, name }) overrides the single-target env.
  let targets;
  if (process.env.PERSEA_E2E_SSH_TARGETS) {
    try {
      targets = JSON.parse(process.env.PERSEA_E2E_SSH_TARGETS);
    } catch (err) {
      throw new Error(`PERSEA_E2E_SSH_TARGETS is not valid JSON: ${err.message}`);
    }
  } else {
    targets = [
      {
        host: TARGET_HOST,
        port: TARGET_PORT,
        user: TARGET_USER,
        password: TARGET_PASSWORD,
        name: "default",
      },
    ];
  }

  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { By, until } = require("selenium-webdriver");

  let failed = 0;
  try {
    // Webview login once, then open each target from the connections page.
    await ensureLoginPage(driver);
    await driver.findElement(By.id("username")).sendKeys(EMAIL);
    await driver.findElement(By.id("password")).sendKeys(PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    await waitForText(driver, "Connections");
    await screenshot(driver, "conn-before");

    for (const target of targets) {
      const api = await apiLogin(BASE, EMAIL, PASSWORD);
      const entryName = `Audit SSH ${Date.now()} ${target.name}`;
      const createRes = await api.post("/api/addressbook/folders/shared/Clients/entries", {
        name: entryName,
        type: "ssh",
        hostname: target.host,
        port: target.port,
        username: target.user,
        password: target.password,
      });
      if (!createRes.ok) {
        failed += 1;
        console.error(`connection target ${target.name}: entry create failed: HTTP ${createRes.status}`);
        continue;
      }

      // Select the entry row (by its slug) and Connect.
      const slug = entrySlug(entryName);
      await driver.get(`${BASE}/connections.html`);
      await waitForText(driver, "Connections");
      await driver.wait(
        until.elementLocated(By.css(`.entry-row[data-name="${slug}"]`)),
        10000,
      );
      await driver.findElement(By.css(`.entry-row[data-name="${slug}"]`)).click();
      await driver.wait(until.elementLocated(By.id("detail-connect")), 10000);
      await driver.findElement(By.id("detail-connect")).click();

      // The session client page flips #status to connected when the SSH
      // session is live (the canvas render follows).
      try {
        await driver.wait(until.elementLocated(By.css("#status.connected")), 60000);
        await screenshot(driver, `connection-live-${target.name}`);
        console.log(`connection target ${target.name}: live SSH session verified`);
      } catch (err) {
        failed += 1;
        console.error(`connection target ${target.name} FAIL: ${err.message}`);
      }

      // Best-effort cleanup: drop the test entry so the server address
      // book stays clean across runs.
      try {
        await api.del(
          `/api/addressbook/folders/shared/Clients/entries/${encodeURIComponent(slug)}`,
        );
      } catch {
        // cleanup is best-effort
      }
    }
  } finally {
    await driver.quit();
  }

  if (failed > 0) {
    throw new Error(`${failed} connection target(s) failed`);
  }
};
