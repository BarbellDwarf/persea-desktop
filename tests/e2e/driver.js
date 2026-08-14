// Shared WebDriver helpers for the Persea Desktop E2E suite.
//
// tauri-driver speaks the WebDriver protocol (like geckodriver/chromedriver)
// and drives the Tauri app's webviews. Playwright cannot drive it: Playwright
// only speaks CDP-style transports, not WebDriver. The standard Tauri E2E
// path is a WebDriver client (this suite uses selenium-webdriver).
//
// Usage: run-specs.js starts tauri-driver itself (it must be on PATH), then
// runs each spec with a fresh session against the built app.

const { Builder } = require("selenium-webdriver");
const { spawn } = require("child_process");
const { mkdirSync } = require("fs");

const APPS_DIR = process.env.PERSEA_E2E_APPS_DIR || "target/release";
const APP_NAME = process.platform === "win32"
  ? "persea-desktop.exe"
  : "persea-desktop";
const APP_PATH = `${APPS_DIR}/${APP_NAME}`;

let driverProcess = null;

function startDriver() {
  const fs = require("fs");
  const log = fs.openSync("tauri-driver.log", "w");
  driverProcess = spawn("tauri-driver", ["--port", "4444"], {
    stdio: ["ignore", log, log],
  });
  // tauri-driver needs a moment to bind the port; poll instead of a
  // fixed sleep so a slow start is tolerated and failures are visible.
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const probe = () => {
      const net = require("net");
      const socket = net.connect(4444, "127.0.0.1");
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
          reject(new Error(`tauri-driver did not bind port 4444:\n${tail}`));
        } else {
          setTimeout(probe, 250);
        }
      });
    };
    probe();
  });
}

async function newSession() {
  const builder = new Builder()
    .usingServer("http://127.0.0.1:4444")
    .withCapabilities({ "tauri:options": { application: APP_PATH } })
    .forBrowser("wry");
  return builder.build();
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
  return driver.takeScreenshot().then((png) => {
    const fs = require("fs");
    fs.writeFileSync(`${dir}/${name}.png`, Buffer.from(png, "base64"));
  });
}

module.exports = { startDriver, stopDriver, newSession, screenshot };
