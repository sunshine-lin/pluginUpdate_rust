#!/usr/bin/env bash
# 从 GitHub Actions 取最新构建的安装包，放进插件仓库的 updater 目录。
#
# 背景：本机（macOS）构建不出 Windows 包——只装了 aarch64-apple-darwin，
# 没有 mingw/cargo-xwin 交叉编译工具链。故 .exe 由 GitHub Actions 的
# windows-latest job 构建并签名（签名在打包阶段完成，拿不到私钥会直接构建失败，
# 见 .github/workflows/build.yml 的注释）。
#
# 用法:
#   scripts/fetch-release.sh              # 取当前 HEAD 那次构建
#   scripts/fetch-release.sh --publish    # 顺带更新 latest.json（会让全部机器自动升级）
#   scripts/fetch-release.sh --wait       # CI 还在跑时等它完成
#
# 依赖 GH_TOKEN（写在 ~/.zshrc 里）。

set -euo pipefail

REPO="sunshine-lin/pluginUpdate_rust"
PLUGIN_UPDATER_DIR="$HOME/cj/pms-aichat/public/updater"
ARTIFACT_NAME="updater-release"
# 单次下载的超时。4MB 的包在公司网络约 30~60 秒，给足余量
DOWNLOAD_TIMEOUT=600

PUBLISH=0
WAIT=0
for arg in "$@"; do
  case "$arg" in
    --publish) PUBLISH=1 ;;
    --wait) WAIT=1 ;;
    *) echo "未知参数: $arg" >&2; exit 2 ;;
  esac
done

if [ -z "${GH_TOKEN:-}" ]; then
  echo "错误: 未设置 GH_TOKEN。artifact 下载必须认证（即使公开仓库也是）。" >&2
  echo "把 export GH_TOKEN=... 写进 ~/.zshrc 后重开终端。" >&2
  exit 2
fi

api() { curl -s --max-time 30 -H "Authorization: token $GH_TOKEN" "$@"; }

SHA=$(git rev-parse HEAD)
SHORT=${SHA:0:7}
echo "本地 HEAD: $SHORT"

# ── 找到对应这次提交的 CI 运行 ────────────────────────────────
find_run() {
  api "https://api.github.com/repos/$REPO/actions/runs?per_page=20" | python3 -c "
import sys, json
sha = '$SHA'
d = json.load(sys.stdin)
for r in d.get('workflow_runs', []):
    if r['head_sha'] == sha:
        print(r['id'], r['status'], r.get('conclusion') or '-')
        break
"
}

INFO=$(find_run)
if [ -z "$INFO" ]; then
  echo "错误: 没找到 $SHORT 对应的 CI 运行。可能还没推送，或推的是别的分支。" >&2
  exit 1
fi
RUN_ID=$(echo "$INFO" | awk '{print $1}')
STATUS=$(echo "$INFO" | awk '{print $2}')
CONCLUSION=$(echo "$INFO" | awk '{print $3}')

# ── 必要时等 CI 完成 ──────────────────────────────────────────
if [ "$STATUS" != "completed" ]; then
  if [ "$WAIT" -eq 0 ]; then
    echo "CI 仍在进行中（$STATUS）。加 --wait 等它完成，或稍后重跑。" >&2
    exit 1
  fi
  echo "CI 进行中，等待完成…"
  # Windows job 实测 10~15 分钟；30 秒轮询一次，最多等 40 分钟
  for _ in $(seq 1 80); do
    sleep 30
    INFO=$(find_run)
    STATUS=$(echo "$INFO" | awk '{print $2}')
    CONCLUSION=$(echo "$INFO" | awk '{print $3}')
    echo "  状态: $STATUS ${CONCLUSION}"
    [ "$STATUS" = "completed" ] && break
  done
fi

if [ "$CONCLUSION" != "success" ]; then
  echo "错误: CI 未成功（conclusion=$CONCLUSION）。先去 Actions 页面看失败原因。" >&2
  echo "https://github.com/$REPO/actions/runs/$RUN_ID" >&2
  exit 1
fi
echo "CI 构建成功（run $RUN_ID）"

# ── 下载 artifact ─────────────────────────────────────────────
ART_URL=$(api "https://api.github.com/repos/$REPO/actions/runs/$RUN_ID/artifacts" | python3 -c "
import sys, json
d = json.load(sys.stdin)
for a in d.get('artifacts', []):
    if a['name'] == '$ARTIFACT_NAME':
        print(a['archive_download_url'])
        break
")
if [ -z "$ART_URL" ]; then
  echo "错误: 这次运行里没有 $ARTIFACT_NAME artifact。" >&2
  exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
echo "下载中…"
curl -sL --max-time "$DOWNLOAD_TIMEOUT" -H "Authorization: token $GH_TOKEN" \
  -o "$TMP/release.zip" "$ART_URL"
unzip -o -q "$TMP/release.zip" -d "$TMP"

EXE=$(find "$TMP" -name "*-setup.exe" | head -1)
SIG=$(find "$TMP" -name "*-setup.exe.sig" | head -1)
if [ -z "$EXE" ] || [ -z "$SIG" ]; then
  echo "错误: artifact 里缺少 .exe 或 .sig。" >&2
  ls -la "$TMP" >&2
  exit 1
fi

VERSION=$(basename "$EXE" | sed -E 's/.*_([0-9]+\.[0-9]+\.[0-9]+)_.*/\1/')
echo "拿到 v$VERSION"

# ── 校验版本号与代码一致 ──────────────────────────────────────
# 防止「改了版本号但推的是旧 commit」这类错位：装上去的包和以为的不是一个
CONF_VERSION=$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
if [ "$VERSION" != "$CONF_VERSION" ]; then
  echo "错误: 包版本 $VERSION 与 tauri.conf.json 的 $CONF_VERSION 不一致。" >&2
  exit 1
fi

mkdir -p "$PLUGIN_UPDATER_DIR"
cp "$EXE" "$PLUGIN_UPDATER_DIR/"
cp "$SIG" "$PLUGIN_UPDATER_DIR/"
echo "已放入 $PLUGIN_UPDATER_DIR"

# ── 可选：更新 latest.json ────────────────────────────────────
if [ "$PUBLISH" -eq 1 ]; then
  echo "更新 latest.json → v$VERSION"
  EXE_NAME=$(basename "$EXE")
  python3 - "$PLUGIN_UPDATER_DIR" "$VERSION" "$EXE_NAME" <<'PYEOF'
import json, os, sys, urllib.parse, datetime
d, version, exe_name = sys.argv[1], sys.argv[2], sys.argv[3]
manifest = os.path.join(d, 'latest.json')
with open(manifest, encoding='utf-8') as f:
    m = json.load(f)
with open(os.path.join(d, exe_name + '.sig'), encoding='utf-8') as f:
    sig = f.read().strip()
m['version'] = version
# pub_date 用 UTC 带 Z 后缀，与既有格式一致
m['pub_date'] = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%f')[:-3] + 'Z'
m['platforms']['windows-x86_64'] = {
    'signature': sig,
    # 文件名含空格，必须百分号编码——否则客户端下载会 404
    'url': 'https://chainai.cjdropshipping.cn/updater/' + urllib.parse.quote(exe_name),
}
with open(manifest, 'w', encoding='utf-8') as f:
    json.dump(m, f, ensure_ascii=False, indent=2)
print('  版本:', version)
print('  注意: notes 字段未自动改，需要手工写这次的更新说明（面向采购同事的白话）')
PYEOF
  echo
  echo "⚠️  latest.json 已改。提交推送后，全部机器会在下次检查时自动升级。"
  echo "    发布前记得手工写 notes（面向采购同事的白话说明）。"
else
  echo
  echo "latest.json 未改（不会触发任何机器升级）。"
  echo "确认这个包没问题后，加 --publish 再跑一次即可发布。"
fi
