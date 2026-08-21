// Shared WebDriver helpers for the Persea Desktop E2E suite.
//
// tauri-driver speaks the WebDriver protocol (like geckodriver/chromedriver)
// and drives the Tauri app's webviews. Playwright cannot drive it: Playwright
// only speaks CDP-style transports, not WebDriver. The standard Tauri E2E
// path is a WebDriver client (this suite uses selenium-webdriver).
//
// Driver backends per platform:
//   Linux/Windows: tauri-driver spawns the native WebDriver (WebKitWebDriver
//     / msedgedriver), which launches the app. tauri-driver must be on PATH.
//   macOS: tauri-driver has no macOS support; the debug app embeds a
//     WebDriver server (tauri-plugin-wdio-webdriver) that starts when
//     TAURI_WEBDRIVER_PORT is set. We spawn the app directly and connect
//     to that port.

const { Builder } = require("selenium-webdriver");
const { spawn } = require("child_process");
const { mkdirSync, writeFileSync } = require("fs");

const APPS_DIR = process.env.PERSEA_E2E_APPS_DIR || "target/release";
const APP_NAME = process.platform === "win32"
  ? "persea-desktop.exe"
  : "persea-desktop";
const APP_PATH = `${APPS_DIR}/${APP_NAME}`;
const IS_MACOS = process.platform === "darwin";
const DRIVER_PORT = IS_MACOS ? 4445 : 4444;

let driverProcess = null;

function startDriver() {
  if (IS_MACOS) {
    // The embedded server lives inside the app; newSession spawns it.
    return Promise.resolve();
  }
  const fs = require("fs");
  const log = fs.openSync("tauri-driver.log", "w");
  driverProcess = spawn("tauri-driver", ["--port", String(DRIVER_PORT)], {
    stdio: ["ignore", log, log],
  });
  // The driver server needs a moment to bind the port; poll instead of a
  // fixed sleep so a slow start is tolerated and failures are visible.
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const probe = () => {
      const net = require("net");
      const socket = net.connect(DRIVER_PORT, "127.0.0.1");
      socket.on("connect", () => {
        socket.destroy();
        resolve();
      });
      socket.on("error", () => {
        socket.destroy();
        if (Date.now() > deadline) {
          const fs = require("fs");
          const tail = fs.existsSync("tauri-driver.log")
            ? fs.readFileSync("tauri-driver.log", "utf8").split("\n").slice(-10).join("\n")
            : "(no log)";
          reject(new Error(`WebDriver server did not bind port ${DRIVER_PORT}:\n${tail}`));
        } else {
          setTimeout(probe, 250);
        }
      });
    };
    probe();
  });
}

// Restart the app on macOS so the fresh process reads the store the spec
// just seeded. The embedded WebDriver server survives session deletion,
// so a stale instance would keep its old in-memory store otherwise.
function waitForPort(acceptDeadlineMs, expect) {
  // Polls the driver port until it accepts (`"open"`) or refuses
  // (`"closed"`) connections. The closed wait matters after a kill: the
  // dying app's socket can still accept for a moment, and a session
  // built against it dies with ECONNREFUSED mid-spec.
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + acceptDeadlineMs;
    const probe = () => {
      const net = require("net");
      const socket = net.connect(DRIVER_PORT, "127.0.0.1");
      socket.on("connect", () => {
        socket.destroy();
        if (expect === "open") {
          resolve();
        } else if (Date.now() > deadline) {
          reject(new Error(`port ${DRIVER_PORT} is still in use; the previous app did not release it`));
        } else {
          setTimeout(probe, 250);
        }
      });
      socket.on("error", () => {
        socket.destroy();
        if (expect === "closed") {
          resolve();
        } else if (Date.now() > deadline) {
          const fs = require("fs");
          const tail = fs.existsSync("tauri-driver.log")
            ? fs.readFileSync("tauri-driver.log", "utf8").split("\n").slice(-10).join("\n")
            : "(no log)";
          reject(new Error(`app WebDriver server did not bind port ${DRIVER_PORT}:\n${tail}`));
        } else {
          setTimeout(probe, 250);
        }
      });
    };
    probe();
  });
}

async function waitForExit(pid, timeoutMs) {
  // The process is gone when kill(pid, 0) reports ESRCH. Waiting for the
  // full exit (not just the socket closing) matters: a shutting-down app
  // saves its instance store on the way out and would clobber a seed
  // written too early (persea-desktop#104).
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      process.kill(pid, 0);
    } catch (_) {
      return;
    }
    if (Date.now() > deadline) {
      throw new Error(`app pid ${pid} did not exit within ${timeoutMs}ms`);
    }
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function restartApp() {
  if (driverProcess) {
    const pid = driverProcess.pid;
    // SIGTERM first so a live app can shut down cleanly; if it is still
    // holding the driver port after a short grace period, SIGKILL. The
    // port MUST be released before the replacement spawns: a session
    // attached to the dying app dies with ECONNREFUSED mid-spec.
    try {
      driverProcess.kill();
    } catch (_) {
      // already gone
    }
    await waitForPort(5000, "closed").catch(() => {});
    try {
      driverProcess.kill("SIGKILL");
    } catch (_) {
      // already gone
    }
    await waitForPort(10000, "closed").catch(() => {});
    if (pid) {
      await waitForExit(pid, 10000).catch(() => {});
    }
    driverProcess = null;
  }
  applyPendingSeed();
  const fs = require("fs");
  const log = fs.openSync("tauri-driver.log", "w");
  driverProcess = spawn(APP_PATH, [], {
    env: { ...process.env, TAURI_WEBDRIVER_PORT: String(DRIVER_PORT) },
    stdio: ["ignore", log, log],
  });
  await waitForPort(20000, "open");
}

async function newSession() {
  if (IS_MACOS) {
    await restartApp();
  } else {
    applyPendingSeed();
  }
  const builder = new Builder()
    .usingServer(`http://127.0.0.1:${DRIVER_PORT}`)
    .withCapabilities({
      "tauri:options": { application: APP_PATH },
      "wdio:tauriServiceOptions": { windowLabel: "main" },
    })
    .forBrowser("wry");
  // The driver can die between the port probe and the session build
  // (macOS: the previous app instance still holding the port), so
  // retry the build on connection errors instead of failing the spec.
  let driver = null;
  const buildDeadline = Date.now() + 20000;
  for (;;) {
    try {
      // The build is async; without the await the rejection escapes the
      // try/catch and the retry never happens.
      driver = await builder.build();
      break;
    } catch (e) {
      if (Date.now() > buildDeadline) {
        throw e;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  // The WebDriver server binds before the app finishes startup (main
  // window, webview, initial navigation), so early commands can hit a
  // half-created page and the startup can reload the webview under the
  // session. Wait until the page is present and stable across two
  // reads before handing the driver out, on every platform.
  const deadline = Date.now() + 30000;
  for (;;) {
    const first = await driver
      .executeScript(
        "return { url: location.href, ready: document.readyState, form: !!document.getElementById('welcome-form') }",
      )
      .catch(() => null);
    await new Promise((r) => setTimeout(r, 600));
    const second = await driver
      .executeScript("return { url: location.href, ready: document.readyState }")
      .catch(() => null);
    const stable =
      first &&
      second &&
      first.url === second.url &&
      first.ready === "complete" &&
      second.ready === "complete";
    if (stable || Date.now() > deadline) {
      break;
    }
  }
  return driver;
}

async function stopDriver() {
  if (driverProcess) {
    driverProcess.kill();
    driverProcess = null;
  }
}

function screenshot(driver, name) {
  const dir = process.env.PERSEA_E2E_SHOTS || "docs/screenshots";
  mkdirSync(dir, { recursive: true });
  // Headless containers render without a GPU (WebKitGTK logs DRI3
  // errors and falls back to software compositing), so the compositor
  // can lag a navigation: a capture right after a DOM wait sometimes
  // returns the previous frame. Settle before capturing.
  return new Promise((resolve) => setTimeout(resolve, 1500)).then(() =>
    driver.takeScreenshot().then((png) => {
      const fs = require("fs");
      fs.writeFileSync(`${dir}/${name}.png`, Buffer.from(png, "base64"));
    }),
  );
}

// Pre-seed the shell's instance store before the app launches. The
// navigation lockdown only allows origins present in the store.
//
// The write is deferred to the next spawn: a still-shutting-down app
// saves its own store on exit and would clobber a seed written before
// it died (persea-desktop#104). newSession applies the seed after the
// old process is gone and before the replacement spawns.
let pendingSeed = null;

function instanceStorePath() {
  const { homedir } = require("os");
  const { join } = require("path");
  const configDir = process.platform === "win32"
    ? join(process.env.APPDATA, "dev.persea.desktop")
    : process.platform === "darwin"
      ? join(homedir(), "Library", "Application Support", "dev.persea.desktop")
      : join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "dev.persea.desktop");
  mkdirSync(configDir, { recursive: true });
  return join(configDir, "instances.json");
}

function seedInstances(instances) {
  pendingSeed = JSON.stringify({
    instances,
    lastUsed: instances.find((i) => i.default)?.url || null,
  });
}

function applyPendingSeed() {
  if (pendingSeed === null) return;
  writeFileSync(instanceStorePath(), pendingSeed);
  pendingSeed = null;
}

module.exports = { startDriver, stopDriver, newSession, screenshot, seedInstances };
