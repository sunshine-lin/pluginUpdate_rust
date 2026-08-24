#!/usr/bin/env bash
# 从 GitHub Actions 取最新构建的安装包，放进插件仓库的 updater 目录。
#
# 背景：本机（macOS）构建不出 Windows 包——只装了 aarch64-apple-darwin，
# 没有 mingw/cargo-xwin 交叉编译工具链。故 .exe 由 GitHub Actions 的
# windows-latest job 构建并签名（签名在打包阶段完成，拿不到私钥会直接构建失败，
# 见 .github/workflows/build.yml 的注释）。
#
# 用法:
#   scripts/fetch-release.sh                          # 取当前 HEAD 那次构建
#   scripts/fetch-release.sh --wait                   # CI 还在跑时等它完成
#   scripts/fetch-release.sh --publish --notes-file N # 发布：改 latest.json + 提交推送打 tag
#   scripts/fetch-release.sh --publish ... --dry-run  # 只看会做什么，不真写
#
# --publish 会走完整条发布链路（插件仓库那边也自动做完）：
#   更新 latest.json → git add/commit → push release → 打 tag → push tag
#
# 依赖 GH_TOKEN（写在 ~/.zshrc 里）。
#
# # 为什么 --publish 强制要求 --notes-file
# notes 是**给采购同事看**的更新说明，脚本没法替人写。早先的做法是发布后打印
# 一句「记得手工改 notes」——2026-08-24 发 0.3.1 时就漏了：latest.json 里躺着
# 0.2.6 的说明（讲「机器状态页」「时间早 8 小时」），与本次改动毫无关系。
# 靠人记得的提醒迟早会漏，所以改成缺了就不让发，让这类错在结构上无法发生。

set -euo pipefail

REPO="sunshine-lin/pluginUpdate_rust"
PLUGIN_REPO="$HOME/cj/pms-aichat"
PLUGIN_UPDATER_DIR="$PLUGIN_REPO/public/updater"
# 插件仓库的线上分支。updater 目录由它对外提供下载，故发布必须落在这条分支上
PLUGIN_RELEASE_BRANCH="release"
ARTIFACT_NAME="updater-release"
# 单次下载的超时。4MB 的包在公司网络约 30~60 秒，给足余量
DOWNLOAD_TIMEOUT=600

PUBLISH=0
WAIT=0
DRY_RUN=0
NOTES_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --publish) PUBLISH=1 ;;
    --wait) WAIT=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --notes-file)
      shift
      NOTES_FILE="${1:-}"
      [ -n "$NOTES_FILE" ] || { echo "错误: --notes-file 后面要跟文件路径。" >&2; exit 2; }
      ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$PUBLISH" -eq 1 ]; then
  if [ -z "$NOTES_FILE" ]; then
    echo "错误: --publish 必须带 --notes-file <文件>。" >&2
    echo "  notes 是给采购同事看的更新说明，脚本写不出来；不强制就会像 0.3.1 那次" >&2
    echo "  一样，把上一版的说明原样发出去。" >&2
    exit 2
  fi
  if [ ! -s "$NOTES_FILE" ]; then
    echo "错误: notes 文件不存在或为空: $NOTES_FILE" >&2
    exit 2
  fi
fi

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

# ── 可选：发布（改 latest.json + 插件仓库提交推送打 tag）──────────
if [ "$PUBLISH" -ne 1 ]; then
  echo
  echo "latest.json 未改（不会触发任何机器升级）。"
  echo "确认这个包没问题后，加 --publish --notes-file <文件> 即可发布。"
  exit 0
fi

# 发布落在插件仓库的线上分支上。不自动切分支：切换可能牵扯到未提交改动，
# 由人确认比脚本自作主张安全
CUR_BRANCH=$(git -C "$PLUGIN_REPO" rev-parse --abbrev-ref HEAD)
if [ "$CUR_BRANCH" != "$PLUGIN_RELEASE_BRANCH" ]; then
  echo "错误: 插件仓库当前在 $CUR_BRANCH，发布必须在 $PLUGIN_RELEASE_BRANCH 上。" >&2
  echo "  先 git -C $PLUGIN_REPO checkout $PLUGIN_RELEASE_BRANCH 再重跑。" >&2
  exit 1
fi

echo "更新 latest.json → v$VERSION"
EXE_NAME=$(basename "$EXE")
python3 - "$PLUGIN_UPDATER_DIR" "$VERSION" "$EXE_NAME" "$NOTES_FILE" "$DRY_RUN" <<'PYEOF'
import json, os, sys, urllib.parse, datetime
d, version, exe_name, notes_file, dry = sys.argv[1:6]
manifest = os.path.join(d, 'latest.json')
with open(manifest, encoding='utf-8') as f:
    m = json.load(f)
with open(os.path.join(d, exe_name + '.sig'), encoding='utf-8') as f:
    sig = f.read().strip()
notes = open(notes_file, encoding='utf-8').read().strip()
m['version'] = version
m['notes'] = notes
# pub_date 用 UTC 带 Z 后缀，与既有格式一致
m['pub_date'] = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%f')[:-3] + 'Z'
m['platforms']['windows-x86_64'] = {
    'signature': sig,
    # 文件名含空格，必须百分号编码——否则客户端下载会 404
    'url': 'https://chainai.cjdropshipping.cn/updater/' + urllib.parse.quote(exe_name),
}
if dry == '1':
    print('  [dry-run] 不写入。latest.json 将变成:')
    for line in json.dumps(m, ensure_ascii=False, indent=2).splitlines():
        print('    ' + line[:160])
else:
    with open(manifest, 'w', encoding='utf-8') as f:
        json.dump(m, f, ensure_ascii=False, indent=2)
    print('  版本:', version)
    print('  说明:', notes.splitlines()[0][:60] + ('…' if len(notes) > 60 else ''))
PYEOF

# tag 形如 8.24.1738（月.日.时分，均不补零），对应上线时间点 UTC+8
TAG=$(date '+%-m.%-d.%H%M')

if [ "$DRY_RUN" -eq 1 ]; then
  echo
  echo "[dry-run] 接下来会在 $PLUGIN_REPO 做:"
  echo "  git add public/updater"
  echo "  git commit -m 'chore(updater): 发布客户端 $VERSION'"
  echo "  git push origin $PLUGIN_RELEASE_BRANCH"
  echo "  git tag $TAG && git push origin $TAG"
  exit 0
fi

# 只提交 updater 目录：插件仓库里可能有别人正在改的东西，
# 一个 git add -A 会把无关改动一起带上线
git -C "$PLUGIN_REPO" add public/updater
if git -C "$PLUGIN_REPO" diff --cached --quiet; then
  echo "updater 目录无变化，跳过提交。"
else
  git -C "$PLUGIN_REPO" commit -q -m "chore(updater): 发布客户端 $VERSION

由 pluginUpdate_rust 的 scripts/fetch-release.sh 自动生成。
安装包来自 CI run $RUN_ID（commit $SHORT），版本已与 tauri.conf.json 校验一致。

日常: 客户端发版，随 pluginUpdate_rust 侧工单一并记录"
  echo "已提交: $(git -C "$PLUGIN_REPO" log -1 --oneline)"
fi

git -C "$PLUGIN_REPO" push origin "$PLUGIN_RELEASE_BRANCH"

if git -C "$PLUGIN_REPO" rev-parse "$TAG" >/dev/null 2>&1; then
  echo "tag $TAG 已存在，跳过打 tag（同一分钟内重复发布）。"
else
  git -C "$PLUGIN_REPO" tag "$TAG"
  git -C "$PLUGIN_REPO" push origin "$TAG"
  echo "已打 tag: $TAG"
fi

echo
echo "✅ 已推送 v$VERSION（tag $TAG）"
echo
# 推送 ≠ 上线：插件仓库由 GitLab CI 构建镜像再部署，实测约 4 分钟。
# 这里主动等一下并核实，否则「推完就以为发布好了」——而线上还是旧的
echo "等待 GitLab CI 部署（推送不等于上线，实测约 4 分钟）…"
MANIFEST_URL="https://chainai.cjdropshipping.cn/updater/latest.json"
DEPLOYED=0
for _ in $(seq 1 20); do
  sleep 30
  LIVE=$(curl -s --max-time 20 "$MANIFEST_URL" \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('version',''))" 2>/dev/null || echo "")
  echo "  线上 latest.json 版本: ${LIVE:-取不到}"
  if [ "$LIVE" = "$VERSION" ]; then DEPLOYED=1; break; fi
done

if [ "$DEPLOYED" -eq 1 ]; then
  echo "✅ 线上已是 v$VERSION，机器会在下次检查（最长 4 小时）时尝试升级。"
else
  echo "⚠️  等了 10 分钟线上仍不是 v$VERSION。去 GitLab 看部署流水线是否失败；" >&2
  echo "    在它上线前，没有任何机器会看到这个版本。" >&2
fi
echo
echo "核实各台是否真的升上来: 查每台的 client 日志有没有"
echo "「客户端启动，版本 $VERSION」——旧版没有这条就是没升上来。"
echo "  python3 ~/.claude/skills/aichat-logs/aichat_logs.py --scan"
