// Runs every spec in tests/e2e/specs against the built app via tauri-driver.
// Env (set by the CI workflow or provision-server.sh):
//   PERSEA_E2E_BASE_URL, PERSEA_E2E_API_KEY, PERSEA_E2E_APPS_DIR, PERSEA_E2E_SHOTS
const { startDriver, stopDriver } = require("./driver");
const { readdirSync } = require("fs");
const { join } = require("path");

async function main() {
  if (!process.env.PERSEA_E2E_BASE_URL) {
    console.error("PERSEA_E2E_BASE_URL is required");
    process.exit(1);
  }
  await startDriver();
  const specs = readdirSync(join(__dirname, "specs"))
    .filter((f) => f.endsWith(".spec.js"))
    .sort();
  let failed = 0;
  for (const spec of specs) {
    console.log(`\n=== ${spec} ===`);
    try {
      await require(join(__dirname, "specs", spec))();
      console.log(`PASS ${spec}`);
    } catch (err) {
      failed += 1;
      console.error(`FAIL ${spec}: ${err.message}`);
      console.error(err.stack);
    }
  }
  await stopDriver();
  if (failed > 0) {
    console.error(`\n${failed} spec(s) failed`);
    process.exit(1);
  }
  console.log("\nAll specs passed");
}

main();
