#!/usr/bin/env node
// 从构建产物生成 latest.json（tauri-plugin-updater 消费的更新清单）。
//
// 用法:
//   node scripts/generate-latest-json.cjs <bundleDir> <version> <baseUrl> <outFile> [notes]
//
// bundleDir 下需存在 NSIS 安装包 *-setup.exe 及其同名 .sig 签名文件
// （由 tauri build 在 bundle.createUpdaterArtifacts=true 且提供签名私钥时产出）。
//
// 字段结构与 src-tauri/src/updater_manifest.rs 保持一致，那边有单测覆盖。

const fs = require("fs");
const path = require("path");

function fail(msg) {
  console.error(`[generate-latest-json] ${msg}`);
  process.exit(1);
}

const [, , bundleDir, version, baseUrl, outFile, notes] = process.argv;

if (!bundleDir || !version || !baseUrl || !outFile) {
  fail(
    "参数不足。用法: generate-latest-json.cjs <bundleDir> <version> <baseUrl> <outFile> [notes]"
  );
}

if (!fs.existsSync(bundleDir)) {
  fail(`构建产物目录不存在: ${bundleDir}`);
}

const files = fs.readdirSync(bundleDir);
const setupExe = files.find((f) => f.endsWith("-setup.exe"));
if (!setupExe) {
  fail(
    `未找到 *-setup.exe。目录内容: ${files.join(", ") || "(空)"}\n` +
      `请确认 tauri.conf.json 中 bundle.targets 含 nsis。`
  );
}

const sigFile = `${setupExe}.sig`;
if (!files.includes(sigFile)) {
  // 缺签名说明构建时没拿到私钥。此时若照常生成清单，客户端会因验签失败
  // 静默拒绝更新——宁可让 CI 在这里失败，也不要发布一份装不上的更新。
  fail(
    `未找到签名文件 ${sigFile}。\n` +
      `请确认 CI 设置了 TAURI_SIGNING_PRIVATE_KEY（及密码 TAURI_SIGNING_PRIVATE_KEY_PASSWORD）。`
  );
}

const signature = fs.readFileSync(path.join(bundleDir, sigFile), "utf8").trim();

const manifest = {
  version,
  notes: notes || `aichat Updater ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${baseUrl.replace(/\/+$/, "")}/${setupExe}`,
    },
  },
};

fs.mkdirSync(path.dirname(path.resolve(outFile)), { recursive: true });
fs.writeFileSync(outFile, JSON.stringify(manifest, null, 2), "utf8");

console.log(`[generate-latest-json] 已生成 ${outFile}`);
console.log(JSON.stringify(manifest, null, 2));
