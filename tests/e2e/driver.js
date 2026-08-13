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
const APP_NAME = process.platform === "darwin"
  ? "persea-desktop.app"
  : process.platform === "win32"
    ? "persea-desktop.exe"
    : "persea-desktop";
const APP_PATH = process.platform === "darwin"
  ? `${APPS_DIR}/bundle/macos/${APP_NAME}`
  : process.platform === "win32"
    ? `${APPS_DIR}/${APP_NAME}`
    : `${APPS_DIR}/${APP_NAME}`;

let driverProcess = null;

function startDriver() {
  driverProcess = spawn("tauri-driver", ["--port", "4444"], {
    stdio: "ignore",
  });
  // tauri-driver needs a moment to bind the port.
  return new Promise((resolve) => setTimeout(resolve, 1500));
}

async function newSession() {
  const builder = new Builder()
    .usingServer("http://127.0.0.1:4444")
    .withCapabilities({ "tauri:options": { application: APP_PATH } });
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
