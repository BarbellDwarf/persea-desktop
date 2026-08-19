/* Persea Desktop login page (D0).
 *
 * The scoped-token sign-in entry point: the shell shows this page when
 * the server requires a fresh sign-in, and it calls cmd_token_acquire
 * with the server URL, username and password. The command is a seam
 * today (the server endpoint lands with persea#227), so the page renders
 * the stub error; once the endpoint exists it renders the acquired
 * token's TTL. Runs after app.js (invoke, escapeHtml).
 *
 * Instance selection: ?url=<instance url> opens the page for a specific
 * instance (the compliance-mode trigger links this way); without the
 * parameter the default instance is used.
 */

function queryInstanceUrl() {
  return new URLSearchParams(window.location.search).get("url");
}

async function resolveInstance() {
  const wanted = queryInstanceUrl();
  let instances = [];
  try {
    instances = await invoke("cmd_instances_list");
  } catch {
    instances = [];
  }
  if (!instances.length) return null;
  if (wanted) {
    return instances.find((i) => i.url === wanted) || instances[0];
  }
  return instances.find((i) => i.default) || instances[0];
}

const form = document.getElementById("login-form");
const instanceInput = document.getElementById("login-instance");
const statusEl = document.getElementById("login-status");
const submitBtn = document.getElementById("login-submit");

async function initPage() {
  const inst = await resolveInstance();
  if (!inst) {
    statusEl.textContent = "No servers configured yet. Add one in Settings first.";
    submitBtn.disabled = true;
    return;
  }
  instanceInput.value = inst.url;
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const username = form.elements.username.value.trim();
  const password = form.elements.password.value;
  if (!username || !password) return;
  submitBtn.disabled = true;
  submitBtn.textContent = "Signing in…";
  statusEl.className = "probe-pending";
  statusEl.textContent = "";
  try {
    const view = await invoke("cmd_token_acquire", {
      url: instanceInput.value,
      username: username,
      password: password,
    });
    const hours = Math.round(view.ttlSecs / 3600);
    statusEl.className = "probe-pending";
    statusEl.textContent =
      "Signed in. The scoped token is valid for " +
      hours +
      " hour" +
      (hours === 1 ? "" : "s") +
      ".";
  } catch (err) {
    statusEl.className = "probe-error";
    statusEl.textContent = String(err);
  } finally {
    form.elements.password.value = "";
    submitBtn.disabled = false;
    submitBtn.textContent = "Log in";
  }
});

initPage();
