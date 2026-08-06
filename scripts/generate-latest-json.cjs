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
  // 缺签名说明签名步骤没跑或失败。此时若照常生成清单，客户端会因验签失败
  // 静默拒绝更新——宁可在这里失败，也不要发布一份装不上的更新。
  fail(
    `未找到签名文件 ${sigFile}。\n` +
      `签名由 scripts/publish-updater.sh 在服务器上执行（tauri signer sign），\n` +
      `请确认私钥路径与 TAURI_SIGNING_PRIVATE_KEY_PASSWORD 配置正确。`
  );
}

const signature = fs.readFileSync(path.join(bundleDir, sigFile), "utf8").trim();

// 文件名需转义后再拼进 URL：productName 是「aichat Updater」（含空格），
// 直接拼出的地址带裸空格，客户端请求会直接失败（实测 curl 无法发出该请求）。
// 只转义文件名本身，不动 baseUrl 的斜杠。
const encodedName = encodeURIComponent(setupExe);

const manifest = {
  version,
  notes: notes || `aichat Updater ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${baseUrl.replace(/\/+$/, "")}/${encodedName}`,
    },
  },
};

fs.mkdirSync(path.dirname(path.resolve(outFile)), { recursive: true });
fs.writeFileSync(outFile, JSON.stringify(manifest, null, 2), "utf8");

console.log(`[generate-latest-json] 已生成 ${outFile}`);
console.log(JSON.stringify(manifest, null, 2));
