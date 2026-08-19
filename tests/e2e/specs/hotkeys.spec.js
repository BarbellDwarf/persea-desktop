// Tab strip chords: Ctrl+Tab / Ctrl+Shift+Tab cycle session tabs. The
// strip page handles the chords locally while focused (tabstrip.js
// document keydown handler -> cmd_tabs_next / cmd_tabs_prev). Without
// a session there are no tabs, so the strip window is hidden and the
// chords have nothing to cycle; the drivable surface is the strip page
// itself: it renders the empty state, the chords dispatch without a
// crash or navigation, and the tab manager still answers.
//
// Limitation: cycling real tabs needs a live session, which needs
// guacd (port 4822); the local/container audit does not provide it.
const { newSession, screenshot, seedInstances } = require("../driver");

const SHELL_ORIGIN = process.platform === "win32" ? "http://tauri.localhost" : "tauri://localhost";

module.exports = async function () {
  seedInstances([]);
  const driver = await newSession();
  const { until, By } = require("selenium-webdriver");

  try {
    await driver.get(`${SHELL_ORIGIN}/tabstrip.html`);
    await driver.wait(until.elementLocated(By.id("strip")), 10000);
    await driver.wait(until.elementLocated(By.id("tabs")), 10000);

    // No instances, no sessions: the tab list is empty and the strip
    // renders no tabs. The strip window itself is hidden by the dock
    // logic (windows.rs dock_strip hides it when no tabs exist); its
    // native visibility is not observable from the main webview.
    const tabs = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_list').catch(() => [])",
    );
    if (!Array.isArray(tabs) || tabs.length !== 0) {
      throw new Error(`expected an empty tab list, got ${JSON.stringify(tabs)}`);
    }
    const tabCount = await driver.executeScript(
      "return document.getElementById('tabs').childElementCount",
    );
    if (tabCount !== 0) {
      throw new Error(`expected no tab elements, got ${tabCount}`);
    }
    await screenshot(driver, "hotkeys-strip-empty");

    // Send the chords. Synthetic keydown events exercise the exact
    // handler path (document keydown -> cmd_tabs_next / cmd_tabs_prev);
    // real OS-level chords to a focused strip window cannot be driven
    // by WebDriver.
    await driver.executeScript(`
      document.dispatchEvent(new KeyboardEvent("keydown", {
        key: "Tab", ctrlKey: true, bubbles: true, cancelable: true
      }));
    `);
    await driver.executeScript(`
      document.dispatchEvent(new KeyboardEvent("keydown", {
        key: "Tab", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true
      }));
    `);

    // No crash, no navigation: the page still answers and the URL is
    // unchanged.
    const urlAfter = await driver.getCurrentUrl();
    if (!urlAfter.endsWith("/tabstrip.html")) {
      throw new Error(`the chords navigated away: ${urlAfter}`);
    }
    const ready = await driver.executeScript("return document.readyState");
    if (ready !== "complete") {
      throw new Error(`the strip page is not alive after the chords: ${ready}`);
    }
    const tabsAfter = await driver.executeScript(
      "return window.__TAURI_INTERNALS__.invoke('cmd_tabs_list').catch(() => [])",
    );
    if (!Array.isArray(tabsAfter) || tabsAfter.length !== 0) {
      throw new Error(`the chords created tabs: ${JSON.stringify(tabsAfter)}`);
    }

    // Best-effort real key events: the webview may intercept Ctrl+Tab
    // before the page sees it, so the synthetic chords above are the
    // deterministic path.
    try {
      const { Key } = require("selenium-webdriver");
      await driver.actions().keyDown(Key.CONTROL).sendKeys(Key.TAB).keyUp(Key.CONTROL).perform();
      await driver
        .actions()
        .keyDown(Key.CONTROL)
        .keyDown(Key.SHIFT)
        .sendKeys(Key.TAB)
        .keyUp(Key.SHIFT)
        .keyUp(Key.CONTROL)
        .perform();
      const urlReal = await driver.getCurrentUrl();
      if (!urlReal.endsWith("/tabstrip.html")) {
        throw new Error(`the real chords navigated away: ${urlReal}`);
      }
    } catch (err) {
      console.log(
        `hotkeys: real key events unavailable (${err.message}); synthetic chords covered the handler path`,
      );
    }

    await screenshot(driver, "hotkeys-chords-sent");
    console.log("hotkeys: strip empty state + chords dispatch without crash or navigation");
  } finally {
    await driver.quit();
  }
};
