// Installed-deb smoke: the packaged app must install and run. The
// audit entrypoint sets PERSEA_E2E_DEB when the deb was built; without
// it the spec skips with a named reason. When present: install the deb
// (dpkg -i), locate the installed binary (dpkg -L), launch it on the
// current DISPLAY, wait for it to stay alive, verify a window appears
// (xdotool when present, else the process-alive check), capture the X
// display when tooling allows, then kill the process and remove the
// package. The installed app is never left running (kill in finally).
const { spawn, execFileSync } = require("child_process");
const { existsSync, mkdirSync } = require("fs");
const { join } = require("path");

const DEB = process.env.PERSEA_E2E_DEB;

function run(cmd, args, opts) {
  return execFileSync(cmd, args, { encoding: "utf8", ...opts });
}

function has(cmd) {
  try {
    execFileSync("sh", ["-c", `command -v ${cmd}`], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

module.exports = async function () {
  if (!DEB) {
    console.log(
      "deb-smoke: skipped, PERSEA_E2E_DEB is not set (the audit entrypoint sets it when the deb was built)",
    );
    return;
  }
  if (!existsSync(DEB)) {
    console.log(`deb-smoke: skipped, the deb does not exist at ${DEB}`);
    return;
  }
  if (!process.env.DISPLAY) {
    console.log("deb-smoke: skipped, no DISPLAY to launch the installed app on");
    return;
  }
  if (typeof process.getuid === "function" && process.getuid() !== 0) {
    console.log("deb-smoke: skipped, dpkg -i needs root");
    return;
  }

  let child = null;
  let installed = false;
  let pkg = null;
  try {
    pkg = run("dpkg-deb", ["-f", DEB, "Package"]).trim();
    if (!pkg) {
      throw new Error("the deb reports no package name");
    }

    // Install, unless the package is already present; only remove what
    // this spec installed.
    let preinstalled = false;
    try {
      run("dpkg", ["-s", pkg], { stdio: "ignore" });
      preinstalled = true;
    } catch {
      // not installed yet
    }
    if (!preinstalled) {
      run("dpkg", ["-i", DEB], { stdio: "inherit" });
      installed = true;
    } else {
      console.log(`deb-smoke: package ${pkg} was already installed; reusing it`);
    }

    // Locate the installed binary.
    const lines = run("dpkg", ["-L", pkg]).split("\n");
    const bin = lines.find((l) => l.includes("/bin/") && existsSync(l));
    if (!bin) {
      throw new Error(`no binary found in dpkg -L ${pkg}`);
    }
    console.log(`deb-smoke: installed binary at ${bin}`);

    // Launch on the current DISPLAY.
    child = spawn(bin, [], { env: { ...process.env }, stdio: "ignore" });
    let spawnError = null;
    child.on("error", (err) => {
      spawnError = err;
    });

    // Wait for it to stay alive (10s).
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline) {
      if (spawnError) {
        throw new Error(`the installed app failed to spawn: ${spawnError.message}`);
      }
      if (child.exitCode !== null) {
        throw new Error(`the installed app exited early with code ${child.exitCode}`);
      }
      await new Promise((r) => setTimeout(r, 500));
    }
    if (child.exitCode !== null) {
      throw new Error(`the installed app exited early with code ${child.exitCode}`);
    }
    console.log("deb-smoke: the installed app stayed alive for 10s");

    // Verify a window appears: xdotool when present, else the
    // process-alive check above is the fallback.
    if (has("xdotool")) {
      const found = run("xdotool", ["search", "--name", "Persea"]).trim();
      if (!found) {
        throw new Error("xdotool found no Persea window");
      }
      console.log(`deb-smoke: xdotool found window id(s) ${found.split("\n").join(", ")}`);
    } else {
      console.log(
        "deb-smoke: xdotool is not available; the process-alive check stands in for the window check",
      );
    }

    // Capture the X display when tooling allows (xwd; convert to png
    // via ffmpeg when present).
    if (has("xwd")) {
      const dir = process.env.PERSEA_E2E_SHOTS || "docs/screenshots";
      mkdirSync(dir, { recursive: true });
      const raw = join(dir, "deb-smoke.xwd");
      try {
        run("xwd", ["-root", "-out", raw], { stdio: "ignore" });
        if (has("ffmpeg")) {
          try {
            run("ffmpeg", ["-y", "-i", raw, join(dir, "deb-smoke.png")], { stdio: "ignore" });
          } catch (e) {
            console.log(
              `deb-smoke: ffmpeg could not convert the xwd capture (${e.message}); the raw xwd is kept`,
            );
          }
        }
        console.log(`deb-smoke: X display captured to ${raw}`);
      } catch (e) {
        console.log(`deb-smoke: xwd capture failed (${e.message})`);
      }
    } else {
      console.log("deb-smoke: xwd is not available; no X capture");
    }

    console.log("deb-smoke: installed-deb path verified");
  } finally {
    if (child) {
      try {
        child.kill("SIGTERM");
      } catch {
        // already gone
      }
      await new Promise((r) => setTimeout(r, 1000));
      if (child.exitCode === null) {
        try {
          child.kill("SIGKILL");
        } catch {
          // already gone
        }
      }
    }
    if (installed && pkg) {
      try {
        run("dpkg", ["-r", pkg], { stdio: "ignore" });
        console.log(`deb-smoke: removed package ${pkg}`);
      } catch (e) {
        console.log(`deb-smoke: could not remove package ${pkg} (${e.message})`);
      }
    }
  }
};
