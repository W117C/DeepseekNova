#!/usr/bin/env node
"use strict";

// bin 包装器：把 npm 包内的平台二进制原样转发给命令行参数。
// 二进制由 postinstall 的 install.js 下载并解压到 ../vendor。

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const binName = process.platform === "win32" ? "deepseeknova-cli.exe" : "deepseeknova-cli";
const binPath = path.join(__dirname, "..", "vendor", binName);

if (!fs.existsSync(binPath)) {
  console.error(
    "DeepseekNova binary not found. Run `npm rebuild deepseeknova` or reinstall the package."
  );
  process.exit(1);
}

const result = spawnSync(binPath, process.argv.slice(2), {
  stdio: "inherit",
});

process.exit(result.status === null ? 1 : result.status);
