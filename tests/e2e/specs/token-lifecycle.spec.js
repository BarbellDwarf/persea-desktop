// Scoped-token lifecycle through the real app (persea-desktop#87/#88/#89):
// acquire via the login page, verify the stored token authenticates after
// a restart (keychain persistence), revoke server-side, and watch the app
// surface the invalidated sign-in. Needs an admin account on the target
// server; skips without credentials like the other credentialed specs.
//
// Not covered here: the 12h TTL boundary (server-enforced; unit-tested in
// token_store.rs) and MFA-in-prompt (out of D1 scope by design; the
// classified error is unit-tested).
const { newSession, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;
const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

async function waitForText(driver, text, timeoutMs = 20000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

async function quietQuit(driver) {
  try {
    await driver.quit();
  } catch (_) {
    // already gone
  }
}

// Count offer banners on the first instance row: .login-offer (the
// probe's auth check failed) and .renew-offer (expired/invalidated).
async function bannerCounts(driver) {
  return driver.executeScript(
    `const row = document.querySelector('.instance-row');
     if (!row) return null;
     return {
       login: row.querySelectorAll('.login-offer').length,
       renew: row.querySelectorAll('.renew-offer').length,
     };`,
  );
}

// Reload Settings until the row's banners satisfy `pred`. Each reload
// re-runs the banner logic against the latest cached probe, so a probe
// landing mid-poll is picked up on the next pass.
async function pollBanners(driver, pred, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    await driver.get(`${SHELL_ORIGIN}/settings.html`);
    await waitForText(driver, "Instances", 10000);
    const counts = await bannerCounts(driver);
    if (counts && pred(counts)) return counts;
    last = counts;
    await new Promise((r) => setTimeout(r, 1500));
  }
  throw new Error(
    `banner state not reached within ${timeoutMs}ms; last counts: ${JSON.stringify(last)}`,
  );
}

// Node-side server client: cookie jar + csrf dance with global fetch.
function cookieHeader(jar) {
  return Object.entries(jar)
    .map(([k, v]) => `${k}=${v}`)
    .join("; ");
}

async function serverLogin(email, password) {
  const jar = {};
  const root = await fetch(`${BASE}/`, { redirect: "manual" });
  const rootCookies = root.headers.getSetCookie ? root.headers.getSetCookie() : [];
  for (const c of rootCookies) {
    const [pair] = c.split(";");
    const idx = pair.indexOf("=");
    jar[pair.slice(0, idx)] = decodeURIComponent(pair.slice(idx + 1));
  }
  const body = new URLSearchParams({
    csrf_token: jar.csrf_token || "",
    username: email,
    password,
  });
  const login = await fetch(`${BASE}/auth/login`, {
    method: "POST",
    redirect: "manual",
    headers: {
      "content-type": "application/x-www-form-urlencoded",
      cookie: cookieHeader(jar),
      "x-csrf-token": jar.csrf_token || "",
    },
    body: body.toString(),
  });
  const loginCookies = login.headers.getSetCookie ? login.headers.getSetCookie() : [];
  for (const c of loginCookies) {
    const [pair] = c.split(";");
    const idx = pair.indexOf("=");
    jar[pair.slice(0, idx)] = decodeURIComponent(pair.slice(idx + 1));
  }
  if (!jar.persea_session) {
    throw new Error(`server login failed (status ${login.status})`);
  }
  return { jar };
}

async function adminListTokens(session, email) {
  const res = await fetch(`${BASE}/api/admin/users/${encodeURIComponent(email)}/tokens`, {
    headers: { cookie: cookieHeader(session.jar) },
  });
  if (!res.ok) throw new Error(`token list failed: ${res.status}`);
  return res.json();
}

async function adminRevokeToken(session, id) {
  const res = await fetch(`${BASE}/api/admin/user-tokens/${id}`, {
    method: "DELETE",
    redirect: "manual",
    headers: {
      cookie: cookieHeader(session.jar),
      "x-csrf-token": session.jar.csrf_token || "",
    },
  });
  if (!res.ok && res.status !== 204) {
    throw new Error(`token revoke failed: ${res.status}`);
  }
}

module.exports = async function () {
  if (!BASE || !EMAIL || !PASSWORD) {
    console.log(
      "token-lifecycle: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set",
    );
    return;
  }
  const { until, By } = require("selenium-webdriver");

  // ── Acquire through the real login page ──
  seedInstances([{ name: "E2E", url: BASE, default: true }]);
  let driver = await newSession();
  await driver.get(`${SHELL_ORIGIN}/login.html?url=${encodeURIComponent(BASE)}`);
  await waitForText(driver, "Log in");
  await driver.findElement(By.id("login-username")).sendKeys(EMAIL);
  await driver.findElement(By.id("login-password")).sendKeys(PASSWORD);
  await driver.findElement(By.id("login-submit")).click();
  await waitForText(driver, "Signed in", 20000);
  console.log("token-lifecycle: scoped token acquired via the login page");
  await quietQuit(driver);

  // ── Persistence + authentication after a restart ──
  // The startup probe presents the stored token, so the auth-failed
  // offer must clear once the fresh probe lands.
  driver = await newSession();
  await pollBanners(driver, (c) => c.login === 0 && c.renew === 0, 25000);
  console.log("token-lifecycle: stored token authenticates after restart (no offer banners)");
  await quietQuit(driver);

  // ── Revoke server-side; the app must surface the invalidation ──
  const session = await serverLogin(EMAIL, PASSWORD);
  const tokens = await adminListTokens(session, EMAIL);
  const scoped = (Array.isArray(tokens) ? tokens : []).find((t) => t.token_type === "scoped");
  if (!scoped) {
    throw new Error("no scoped token found server-side to revoke");
  }
  await adminRevokeToken(session, scoped.id);
  console.log("token-lifecycle: scoped token revoked server-side");

  // A fresh app probes at startup with the revoked token; the probe's
  // auth failure must surface the re-login offer.
  driver = await newSession();
  await pollBanners(driver, (c) => c.login > 0 || c.renew > 0, 30000);
  console.log("token-lifecycle: revocation surfaced the re-login offer");
  await quietQuit(driver);
};
