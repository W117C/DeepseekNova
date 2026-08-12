#!/usr/bin/env node
"use strict";

// DeepseekNova npm 安装器：从 GitHub Releases 下载当前平台二进制并校验
// SHA-256（对齐 install.sh / install.ps1 的资产命名与校验流程）。
//
// 资产命名（release.yml）：
//   deepseeknova-cli-<target>.tar.gz   (macOS / Linux)
//   deepseeknova-cli-<target>.zip      (Windows)
//   checksums.txt                      随 Release 附带

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const crypto = require("crypto");
const { execFileSync } = require("child_process");

const REPO = "W117C/DeepseekNova";
const VERSION = require("./package.json").version;

const TARGETS = {
  "darwin-arm64": { target: "aarch64-apple-darwin", ext: "tar.gz" },
  "darwin-x64": { target: "x86_64-apple-darwin", ext: "tar.gz" },
  "linux-arm64": { target: "aarch64-unknown-linux-gnu", ext: "tar.gz" },
  "linux-x64": { target: "x86_64-unknown-linux-gnu", ext: "tar.gz" },
  "win32-x64": { target: "x86_64-pc-windows-msvc", ext: "zip" },
};

const DRY_RUN = process.argv.includes("--dry-run");
const HELP = process.argv.includes("--help") || process.argv.includes("-h");

if (HELP) {
  console.log(
    "Usage: node install.js [--dry-run]\n\n" +
      "Downloads the DeepseekNova CLI binary for the current platform from\n" +
      "GitHub Releases, verifies its SHA-256, and extracts it into ./vendor."
  );
  process.exit(0);
}

function fail(msg) {
  console.error(`\nInstallation failed: ${msg}`);
  process.exit(1);
}

function platformKey() {
  const key = `${process.platform}-${os.arch()}`;
  if (!TARGETS[key]) {
    fail(
      `unsupported platform/arch '${key}'. Supported: ` +
        Object.keys(TARGETS).join(", ")
    );
  }
  return key;
}

// 跟随 GitHub 302 跳转（release asset URL 会重定向到
// release-assets.githubusercontent.com），最多 5 跳防死循环（对齐 fetchText）。
function download(url, dest, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    const req = https.get(url, { headers: { "User-Agent": "deepseeknova-npm" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        fs.unlinkSync(dest);
        if (redirectsLeft <= 0) {
          reject(new Error(`too many redirects from ${url}`));
          return;
        }
        resolve(download(res.headers.location, dest, redirectsLeft - 1));
        return;
      }
      if (res.statusCode !== 200) {
        file.close();
        fs.unlinkSync(dest);
        reject(new Error(`HTTP ${res.statusCode} from ${url}`));
        return;
      }
      res.pipe(file);
      file.on("finish", () => file.close(resolve));
    });
    req.on("error", (err) => {
      file.close();
      fs.unlinkSync(dest);
      reject(err);
    });
  });
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

// 与 download() 一致地跟随 GitHub 302 跳转（release asset URL 会重定向到
// release-assets.githubusercontent.com），最多 5 跳防死循环。
async function fetchText(url, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "deepseeknova-npm" } }, (res) => {
        if (
          res.statusCode >= 300 &&
          res.statusCode < 400 &&
          res.headers.location
        ) {
          res.resume();
          if (redirectsLeft <= 0) {
            reject(new Error(`too many redirects from ${url}`));
            return;
          }
          resolve(fetchText(res.headers.location, redirectsLeft - 1));
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} from ${url}`));
          return;
        }
        let body = "";
        res.on("data", (c) => (body += c));
        res.on("end", () => resolve(body));
      })
      .on("error", reject);
  });
}

async function main() {
  const key = platformKey();
  const { target, ext } = TARGETS[key];
  const asset = `deepseeknova-cli-${target}.${ext}`;
  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  const url = `${base}/${asset}`;

  const pkgRoot = path.join(__dirname);
  const vendorDir = path.join(pkgRoot, "vendor");

  if (DRY_RUN) {
    console.log(`platform: ${key}`);
    console.log(`asset:    ${asset}`);
    console.log(`url:      ${url}`);
    console.log(`vendor:   ${vendorDir}`);
    process.exit(0);
  }

  fs.mkdirSync(vendorDir, { recursive: true });
  const archive = path.join(vendorDir, asset);

  process.stdout.write(`Downloading ${asset} (v${VERSION})...\n`);
  try {
    await download(url, archive);
  } catch (err) {
    fail(`download failed: ${err.message}\nIs release v${VERSION} published?`);
  }

  process.stdout.write("Verifying SHA-256...\n");
  const checksumsUrl = `${base}/checksums.txt`;
  let expected;
  try {
    const text = await fetchText(checksumsUrl);
    const line = text
      .split(/\r?\n/)
      .find((l) => l.trim().endsWith(`  ${asset}`) || l.trim().endsWith(` ${asset}`));
    if (!line) {
      fail(
        `no SHA-256 checksum entry for ${asset} in checksums.txt ` +
          `(release v${VERSION} may not contain it)`
      );
    }
    expected = line.trim().split(/\s+/)[0].toLowerCase();
  } catch (err) {
    // 与 install.sh / install.ps1 一致：checksums.txt 缺失或不可用即失败，
    // 绝不静默跳过校验。
    fail(
      `failed to fetch checksums.txt for release v${VERSION}: ${err.message}`
    );
  }
  if (expected) {
    const actual = sha256(archive);
    if (actual !== expected) {
      fs.unlinkSync(archive);
      fail(`SHA-256 mismatch\n  expected: ${expected}\n  actual:   ${actual}`);
    }
  }

  process.stdout.write("Extracting...\n");
  try {
    if (ext === "zip") {
      const ps = `Expand-Archive -Force -Path '${archive}' -DestinationPath '${vendorDir}'`;
      execFileSync("powershell", ["-NoProfile", "-Command", ps], { stdio: "ignore" });
    } else {
      execFileSync("tar", ["xzf", archive, "-C", vendorDir], { stdio: "ignore" });
    }
  } catch (err) {
    fail(`extraction failed: ${err.message}`);
  }

  const binName = process.platform === "win32" ? "deepseeknova-cli.exe" : "deepseeknova-cli";
  const binPath = path.join(vendorDir, binName);
  if (!fs.existsSync(binPath)) {
    fail(`expected binary not found after extraction: ${binPath}`);
  }
  if (process.platform !== "win32") {
    fs.chmodSync(binPath, 0o755);
  }
  fs.unlinkSync(archive);

  console.log(`\nInstalled: ${binPath}`);
  console.log(`Run: deepseeknova --help`);
}

main().catch((err) => fail(err.message));
