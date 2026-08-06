#!/usr/bin/env node
/**
 * 从 CHANGELOG.md 取出指定版本的更新说明。
 *
 * 这段文字最终显示给采购同事看（latest.json 的 notes 字段 → 更新提示条），
 * 所以取不到时不报错、只输出空串——由调用方回落，宁可不显示说明，
 * 也不要因为文案缺失而阻断一次本来能正常完成的升级。
 *
 * 用法: node extract-notes.cjs <CHANGELOG路径> <版本号>
 * 输出: 该版本的说明正文（无则空串）
 *
 * 独立成文件而非内联进 shell：内联的 node -e 需要多层反斜杠转义，
 * 极易写错且不易察觉（实测踩过——转义多写一层会静默返回空串）。
 */
const fs = require("fs");

const [, , file, version] = process.argv;
if (!file || !version) process.exit(0); // 参数不全按“无说明”处理，不干扰发布

let out = "";
try {
  const md = fs.readFileSync(file, "utf8");
  const lines = md.split(/\r?\n/);
  // 逐行扫描而不是构造正则：版本号含点号，拼进正则要转义，是上面提到的坑
  const start = lines.findIndex(
    (l) => l.startsWith("## ") && l.slice(3).trim() === version
  );
  if (start !== -1) {
    const rest = lines.slice(start + 1);
    const end = rest.findIndex((l) => l.startsWith("## "));
    out = (end === -1 ? rest : rest.slice(0, end)).join("\n").trim();
  }
} catch {
  // 文件不存在或不可读：同样按“无说明”处理
}

process.stdout.write(out);
