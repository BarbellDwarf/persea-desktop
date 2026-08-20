// Security-function checks: password policy enforcement, brute-force
// lockout, and the disabled-account gate. Needs admin credentials
// (PERSEA_E2E_LOGIN_EMAIL/PASSWORD). Skips without them.
const { newSession, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const ADMIN_EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const ADMIN_PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;

// Cookie-aware API client (node 18 fetch has no jar).
async function apiClient(baseUrl) {
  const jar = {};
  const absorb = (res) => {
    for (const c of res.headers.getSetCookie ? res.headers.getSetCookie() : []) {
      const [pair] = c.split(";");
      const idx = pair.indexOf("=");
      if (idx > 0) jar[pair.slice(0, idx)] = pair.slice(idx + 1);
    }
    return res;
  };
  const cookie = () => Object.entries(jar).map(([k, v]) => `${k}=${v}`).join("; ");
  const base = baseUrl.replace(/\/$/, "");
  return {
    async login(username, password) {
      await absorb(await fetch(`${base}/`, { headers: { cookie: cookie() }, redirect: "manual" }));
      const res = await absorb(
        await fetch(`${base}/auth/login`, {
          method: "POST",
          headers: { "content-type": "application/x-www-form-urlencoded", cookie: cookie() },
          body: new URLSearchParams({ csrf_token: jar.csrf_token || "", username, password }),
          redirect: "manual",
        }),
      );
      return { ok: [302, 303].includes(res.status), location: res.headers.get("location") };
    },
    async request(method, path, body) {
      return fetch(`${base}${path}`, {
        method,
        headers: {
          "content-type": "application/json",
          cookie: cookie(),
          "x-csrf-token": jar.csrf_token || "",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        redirect: "manual",
      });
    },
  };
}

// The login route is rate-limited per IP: retry the admin login a few
// times so a burst limiter cannot trip the setup.
async function adminLoginWithRetry() {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    if (attempt > 0) await new Promise((r) => setTimeout(r, 2500));
    const api = await apiClient(BASE);
    if ((await api.login(ADMIN_EMAIL, ADMIN_PASSWORD)).ok) {
      return api;
    }
  }
  throw new Error("admin login failed (rate limited after retries)");
}

module.exports = async function () {
  if (!ADMIN_EMAIL || !ADMIN_PASSWORD) {
    console.log("security: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set");
    return;
  }
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();

  const results = [];
  const suffix = Date.now();
  const testUser = `security-${suffix}@example.com`;

  const push = (label, ok, detail) => results.push({ label, ok, detail });

  try {
    const api = await adminLoginWithRetry();

    // A disposable user for the account-state cases.
    const created = await api.request("POST", "/api/users", {
      email: testUser,
      name: "Security Test",
      password: "security-user-password-2026",
      role: "viewer",
    });
    if (created.status !== 201) {
      throw new Error(`create security user failed: HTTP ${created.status}`);
    }

    // 1. Password policy: a too-short password is rejected on the update.
    const weak = await api.request("PUT", `/api/users/${encodeURIComponent(testUser)}`, {
      password: "short",
    });
    push("weak password rejected", weak.status === 400, `HTTP ${weak.status}`);

    // 2. Disabled account: disable, then the login is refused. The DB
    //    provider refuses disabled users at auth time, so the verdict is
    //    invalid_credentials rather than account_disabled.
    await api.request("POST", `/api/users/${encodeURIComponent(testUser)}/disable`);
    const disabled = await apiClient(BASE);
    const disabledLogin = await disabled.login(testUser, "security-user-password-2026");
    push(
      "disabled account login refused",
      !disabledLogin.ok || (disabledLogin.location || "").includes("error="),
      `ok=${disabledLogin.ok} location=${disabledLogin.location || "none"}`,
    );
    await api.request("POST", `/api/users/${encodeURIComponent(testUser)}/enable`);

    // 3. Brute-force lockout: repeated failures lock the account. The
    //    login route is also rate-limited per IP, so pace the attempts
    //    and verify the lockout verdict before the final login: a
    //    swallowed attempt (rate-limited) must not leave the account
    //    unlocked.
    for (let i = 0; i < 6; i += 1) {
      await new Promise((r) => setTimeout(r, 2500));
      const attempt = await apiClient(BASE);
      await attempt.login(testUser, "wrong-password-2026");
    }
    let lockedVerdict = false;
    for (let attemptNo = 0; attemptNo < 3 && !lockedVerdict; attemptNo += 1) {
      await new Promise((r) => setTimeout(r, 2500));
      const probe = await apiClient(BASE);
      const probeLogin = await probe.login(testUser, "wrong-password-2026");
      lockedVerdict = (probeLogin.location || "").includes("account_locked");
    }
    await new Promise((r) => setTimeout(r, 2500));
    const locked = await apiClient(BASE);
    const lockedLogin = await locked.login(testUser, "security-user-password-2026");
    // The refusal comes as the account_locked redirect or a 429 from the
    // rate limiter (the burst guard doubles as a lockout shield); both
    // mean the account cannot log in after repeated failures.
    push(
      "lockout after repeated failures",
      lockedVerdict && (!lockedLogin.ok || (lockedLogin.location || "").includes("error=")),
      `verdict=${lockedVerdict} ok=${lockedLogin.ok} location=${lockedLogin.location || "none"}`,
    );
  } catch (err) {
    results.push({ label: "setup", ok: false, detail: err.message });
  } finally {
    try {
      await new Promise((r) => setTimeout(r, 1500));
      const api2 = await adminLoginWithRetry();
      await api2.request("DELETE", `/api/users/${encodeURIComponent(testUser)}`);
    } catch {
      // cleanup is best-effort
    }
    await driver.quit();
  }

  for (const r of results) {
    console.log(`security ${r.label}: ${r.ok ? "PASS" : "FAIL"} (${r.detail})`);
  }
  const failed = results.filter((r) => !r.ok);
  if (failed.length > 0) {
    throw new Error(`${failed.length} security case(s) failed`);
  }
  console.log(`security: ${results.length} cases verified`);
};
