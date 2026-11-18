import { execSync } from "node:child_process";
import { statSync } from "node:fs";

if (process.env.KAIROS_USE_PREBUILT === "1") {
  const web = "crates/kairos-web/dist";
  const outputs = [
    "build/worker/shim.mjs", "build/index.js", "build/index_bg.wasm",
    ...["index.html", "kairos-web.js", "kairos-web_bg.wasm", "boot.js", "style.css", "_headers"]
      .map((name) => `${web}/${name}`),
  ];
  for (const path of outputs) {
    const file = statSync(path, { throwIfNoEntry: false });
    if (!file?.isFile() || file.size === 0) {
      console.error(`Missing build output: ${path}`);
      process.exit(1);
    }
  }
} else {
  execSync("cargo install -q --locked --version 0.9.0 worker-build", { stdio: "inherit" });
  execSync("worker-build --release --panic-unwind --out-dir ../../build crates/kairos-worker", { stdio: "inherit" });
}
