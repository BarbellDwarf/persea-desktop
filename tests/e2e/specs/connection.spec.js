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

// Log in over the API and return { jar, get, post } with the session
// cookie absorbed. node 18 fetch has no cookie jar, so the jar is manual.
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
          headers: { "content-type": "application/json", cookie: cookieHeader() },
          body: JSON.stringify(body),
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
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { By, until } = require("selenium-webdriver");

  try {
    // Create the SSH entry through the API with a fresh name per run.
    const api = await apiLogin(BASE, EMAIL, PASSWORD);
    const entryName = `Audit SSH ${Date.now()}`;
    const createRes = await api.post("/api/addressbook/folders/shared/Clients/entries", {
      name: entryName,
      session_type: "ssh",
      hostname: TARGET_HOST,
      port: TARGET_PORT,
      username: TARGET_USER,
      password: TARGET_PASSWORD,
    });
    if (!createRes.ok) {
      throw new Error(`entry create failed: HTTP ${createRes.status} ${await createRes.text()}`);
    }

    // Webview login, then open the entry from the connections page.
    await waitForText(driver, "Sign in");
    await driver.findElement(By.id("username")).sendKeys(EMAIL);
    await driver.findElement(By.id("password")).sendKeys(PASSWORD);
    await driver.findElement(By.id("login-form")).submit();
    await waitForText(driver, "Connections");
    await screenshot(driver, "conn-before");

    // Select the entry row (matched by its display name) and Connect.
    await driver.wait(
      until.elementLocated(By.xpath(`//*[contains(text(), '${entryName}')]`)),
      10000,
    );
    await driver.findElement(By.xpath(`//*[contains(text(), '${entryName}')]`)).click();
    await driver.wait(until.elementLocated(By.id("detail-connect")), 10000);
    await driver.findElement(By.id("detail-connect")).click();

    // The session client page flips #status to connected when the SSH
    // session is live (the canvas render follows).
    await driver.wait(until.elementLocated(By.css("#status.connected")), 60000);
    await screenshot(driver, "connection-live");

    console.log("connection: live SSH session verified");
  } finally {
    await driver.quit();
  }
};
