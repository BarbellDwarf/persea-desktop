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
function restartApp() {
  if (driverProcess) {
    try {
      driverProcess.kill();
    } catch (_) {
      // already gone
    }
    driverProcess = null;
  }
  const fs = require("fs");
  const log = fs.openSync("tauri-driver.log", "w");
  driverProcess = spawn(APP_PATH, [], {
    env: { ...process.env, TAURI_WEBDRIVER_PORT: String(DRIVER_PORT) },
    stdio: ["ignore", log, log],
  });
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 20000;
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
          reject(new Error(`app WebDriver server did not bind port ${DRIVER_PORT}:\n${tail}`));
        } else {
          setTimeout(probe, 250);
        }
      });
    };
    probe();
  });
}

async function newSession() {
  if (IS_MACOS) {
    await restartApp();
  }
  const builder = new Builder()
    .usingServer(`http://127.0.0.1:${DRIVER_PORT}`)
    .withCapabilities({
      "tauri:options": { application: APP_PATH },
      "wdio:tauriServiceOptions": { windowLabel: "main" },
    })
    .forBrowser("wry");
  const driver = builder.build();
  if (IS_MACOS) {
    // The embedded WebDriver server binds before the app finishes
    // startup (main window, webview, initial navigation), so early
    // commands can hit a half-created page and the startup can reload
    // the webview under the session. Wait until the shell page is
    // present and stable across two reads before handing the driver out.
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
function seedInstances(instances) {
  const { homedir } = require("os");
  const { join } = require("path");
  const configDir = process.platform === "win32"
    ? join(process.env.APPDATA, "dev.persea.desktop")
    : process.platform === "darwin"
      ? join(homedir(), "Library", "Application Support", "dev.persea.desktop")
      : join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "dev.persea.desktop");
  mkdirSync(configDir, { recursive: true });
  writeFileSync(
    join(configDir, "instances.json"),
    JSON.stringify({ instances, lastUsed: instances.find((i) => i.default)?.url || null }),
  );
}

module.exports = { startDriver, stopDriver, newSession, screenshot, seedInstances };
