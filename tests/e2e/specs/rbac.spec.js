// RBAC checks for database and LDAP users: address book access per role,
// entry creation, group-gated folders, and the admin page gate. Needs
// admin credentials (PERSEA_E2E_LOGIN_EMAIL/PASSWORD) and the LDAP users
// (alice has the engineers group, bob has none). Skips without them.
//
// The model under test (server side):
// - address book reads: operator or higher, then group-filtered
// - entry creation: admin or a custom role with create_connection
// - admin pages: admin only
const { newSession, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;
const ADMIN_EMAIL = process.env.PERSEA_E2E_LOGIN_EMAIL;
const ADMIN_PASSWORD = process.env.PERSEA_E2E_LOGIN_PASSWORD;
const LDAP_PASSWORD = process.env.PERSEA_E2E_LDAP_PASSWORD || "alice-ldap-password-2026";

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
    jar,
    async loginResult(username, password) {
      await absorb(await fetch(`${base}/`, { headers: { cookie: cookie() }, redirect: "manual" }));
      const res = await absorb(
        await fetch(`${base}/auth/login`, {
          method: "POST",
          headers: { "content-type": "application/x-www-form-urlencoded", cookie: cookie() },
          body: new URLSearchParams({ csrf_token: jar.csrf_token || "", username, password }),
          redirect: "manual",
        }),
      );
      const location = res.headers.get("location") || "";
      return {
        ok: [302, 303].includes(res.status),
        detail: `HTTP ${res.status} ${location}`,
      };
    },
    async login(username, password) {
      return (await this.loginResult(username, password)).ok;
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

module.exports = async function () {
  if (!ADMIN_EMAIL || !ADMIN_PASSWORD) {
    console.log("rbac: skipped, PERSEA_E2E_LOGIN_EMAIL and PERSEA_E2E_LOGIN_PASSWORD are not set");
    return;
  }
  seedInstances([{ name: "Local", url: BASE, default: true }]);
  const driver = await newSession();
  const { until, By } = require("selenium-webdriver");

  const results = [];
  const suffix = Date.now();
  const dbUsers = {
    viewer: `rbac-viewer-${suffix}@example.com`,
    operator: `rbac-operator-${suffix}@example.com`,
    poweruser: `rbac-poweruser-${suffix}@example.com`,
  };
  const aliceEmail = "alice@example.com";
  const bobEmail = "bob@example.com";
  const admin = { email: ADMIN_EMAIL, password: ADMIN_PASSWORD };
  let folderName = null;

  const runCase = async (label, username, password, method, path, body, wanted) => {
    // The login route is rate-limited per IP (brute-force guard): pace
    // the cases so the burst limiter cannot trip the suite.
    await new Promise((r) => setTimeout(r, 1500));
    const api = await apiClient(BASE);
    const login = await api.loginResult(username, password);
    if (!login.ok) {
      results.push({ label, ok: false, detail: `login failed: ${login.detail}` });
      return;
    }
    const res = await api.request(method, path, body);
    results.push({ label, ok: res.status === wanted, detail: `HTTP ${res.status}` });
  };

  try {
    const api = await apiClient(BASE);
    if (!(await api.login(ADMIN_EMAIL, ADMIN_PASSWORD))) {
      throw new Error("admin login failed");
    }

    // Create the database role users.
    for (const [role, email] of Object.entries(dbUsers)) {
      const res = await api.request("POST", "/api/users", {
        email,
        name: `RBAC ${role}`,
        password: "rbac-user-password-2026",
        role,
      });
      if (res.status !== 201) {
        throw new Error(`create ${role} user failed: HTTP ${res.status}`);
      }
    }

    // Elevate the LDAP users to operator for the group-filter tests.
    for (const email of [aliceEmail, bobEmail]) {
      const res = await api.request("PUT", `/api/users/${encodeURIComponent(email)}/role`, {
        role: "operator",
      });
      if (res.status !== 200) {
        throw new Error(`elevate ${email} failed: HTTP ${res.status}`);
      }
    }

    // Group-gated folder: only the engineers group can see it.
    folderName = `Engineering ${suffix}`;
    const folderRes = await api.request("POST", "/api/addressbook/folders", {
      name: folderName,
      allowed_groups: ["engineers"],
      description: "RBAC test folder",
    });
    if (![200, 201].includes(folderRes.status)) {
      throw new Error(`create gated folder failed: HTTP ${folderRes.status}`);
    }

    // ---- Role matrix on the address book list (operator or higher) ----
    await runCase(
      "viewer (db) folders", dbUsers.viewer, "rbac-user-password-2026",
      "GET", "/api/addressbook/folders", undefined, 403,
    );
    await runCase(
      "operator (db) folders", dbUsers.operator, "rbac-user-password-2026",
      "GET", "/api/addressbook/folders", undefined, 200,
    );
    await runCase(
      "poweruser (db) folders", dbUsers.poweruser, "rbac-user-password-2026",
      "GET", "/api/addressbook/folders", undefined, 200,
    );
    await runCase(
      "alice (ldap) folders", "alice", LDAP_PASSWORD,
      "GET", "/api/addressbook/folders", undefined, 200,
    );
    await runCase(
      "bob (ldap) folders", "bob", "bob-ldap-password-2026",
      "GET", "/api/addressbook/folders", undefined, 200,
    );

    // ---- Entry creation: admin or a create_connection custom role ----
    const entryBody = {
      name: `rbac-entry-${suffix}`,
      type: "ssh",
      hostname: "127.0.0.1",
      port: 22,
      username: "x",
    };
    await runCase(
      "viewer (db) entry create", dbUsers.viewer, "rbac-user-password-2026",
      "POST", "/api/addressbook/folders/shared/Clients/entries", entryBody, 403,
    );
    await runCase(
      "operator (db) entry create", dbUsers.operator, "rbac-user-password-2026",
      "POST", "/api/addressbook/folders/shared/Clients/entries", entryBody, 403,
    );
    await runCase(
      "poweruser (db) entry create", dbUsers.poweruser, "rbac-user-password-2026",
      "POST", "/api/addressbook/folders/shared/Clients/entries", entryBody, 403,
    );
    await runCase(
      "admin entry create", admin.email, admin.password,
      "POST", "/api/addressbook/folders/shared/Clients/entries", entryBody, 201,
    );

    // ---- Admin page gate ----
    await runCase(
      "viewer (db) admin page", dbUsers.viewer, "rbac-user-password-2026",
      "GET", "/admin/settings.html", undefined, 403,
    );
    await runCase(
      "alice (ldap) admin page", "alice", LDAP_PASSWORD,
      "GET", "/admin/settings.html", undefined, 403,
    );
    await runCase(
      "admin admin page", admin.email, admin.password,
      "GET", "/admin/settings.html", undefined, 200,
    );

    // ---- Group-gated folder visibility ----
    // alice (engineers member) sees the gated folder; bob (no groups) does not.
    await new Promise((r) => setTimeout(r, 1500));
    const aliceApi = await apiClient(BASE);
    await aliceApi.login("alice", LDAP_PASSWORD);
    const aliceFolders = await (await aliceApi.request("GET", "/api/addressbook/folders")).json();
    const aliceSees = JSON.stringify(aliceFolders).includes(folderName);
    results.push({ label: "alice (ldap) sees gated folder", ok: aliceSees, detail: aliceSees ? "folder present" : "folder missing" });

    await new Promise((r) => setTimeout(r, 1500));
    const bobApi = await apiClient(BASE);
    await bobApi.login("bob", "bob-ldap-password-2026");
    const bobFolders = await (await bobApi.request("GET", "/api/addressbook/folders")).json();
    const bobSees = JSON.stringify(bobFolders).includes(folderName);
    results.push({ label: "bob (ldap) blocked from gated folder", ok: !bobSees, detail: bobSees ? "folder visible" : "folder absent" });
  } catch (err) {
    results.push({ label: "setup", ok: false, detail: err.message });
  } finally {
    // Restore and clean up as admin.
    try {
      const api2 = await apiClient(BASE);
      await api2.login(ADMIN_EMAIL, ADMIN_PASSWORD);
      for (const u of [aliceEmail, bobEmail]) {
        await api2.request("PUT", `/api/users/${encodeURIComponent(u)}/role`, { role: "viewer" });
      }
      for (const email of Object.values(dbUsers)) {
        await api2.request("DELETE", `/api/users/${encodeURIComponent(email)}`);
      }
      if (folderName) {
        await api2.request(
          "DELETE",
          `/api/addressbook/folders/shared/${encodeURIComponent(folderName)}`,
        );
      }
    } catch {
      // cleanup is best-effort
    }
    await driver.quit();
  }

  for (const r of results) {
    console.log(`rbac ${r.label}: ${r.ok ? "PASS" : "FAIL"} (${r.detail})`);
  }
  const failed = results.filter((r) => !r.ok);
  if (failed.length > 0) {
    throw new Error(`${failed.length} rbac case(s) failed`);
  }
  console.log(`rbac: ${results.length} cases verified`);
};
