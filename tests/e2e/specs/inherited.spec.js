// Inherited web-feature specs: the REMOTE persea UI inside the app's
// webview. Covers the high-risk engine behaviors (login page render,
// docs page render) through the instance viewport. Session-open and
// drive specs need guacd and are exercised in the full CI matrix; they
// degrade to render checks here.
//
// The shell's navigation lockdown only allows origins that exist in the
// instance store, so the spec pre-seeds instances.json (the app's
// config dir) before the session launches and lets the app auto-open
// the default instance.
const { newSession, screenshot, seedInstances } = require("../driver");

const BASE = process.env.PERSEA_E2E_BASE_URL;

async function waitForText(driver, text, timeoutMs = 15000) {
  const { until, By } = require("selenium-webdriver");
  await driver.wait(until.elementLocated(By.xpath(`//*[contains(text(), '${text}')]`)), timeoutMs);
}

module.exports = async function () {
  seedInstances([{ name: "E2E", url: BASE, default: true }]);
  const driver = await newSession();

  try {
    // The default instance opens in the viewport: login page renders.
    await waitForText(driver, "Sign in");
    await screenshot(driver, "web-login");

    // Docs page is public.
    await driver.get(`${BASE}/docs`);
    await waitForText(driver, "persea");
    await screenshot(driver, "web-docs");

    console.log("inherited: login + docs render checks verified");
  } catch (err) {
    try {
      const text = await driver.executeScript(
        "return (document.body && document.body.innerText || '').slice(0, 500)",
      );
      const url = await driver.getCurrentUrl();
      console.error(`viewport text: ${JSON.stringify(text)}`);
      console.error(`viewport url: ${url}`);
    } catch (diagErr) {
      console.error(`diag failed: ${diagErr.message}`);
    }
    throw err;
  } finally {
    await driver.quit();
  }
};
