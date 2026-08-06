#!/usr/bin/env bash
#
# 更新器发布脚本（在分发服务器上执行）
#
# 背景：十几台采购虚拟机的更新器不可能逐台手动替换。本脚本完成发布链路的
# 后半段——把 GitHub Actions 构建好的已签名安装包取回，生成 latest.json，
# 落到 nginx 网站目录，之后各机器自行拉取升级。
#
# 签名为什么不在这里做：tauri 在配置了 pubkey + createUpdaterArtifacts 后，
# 会在打包阶段直接签名并产出 .sig，构建时拿不到私钥就直接失败。
# 因此私钥必须存在 CI（GitHub Secret），产物到达本机时已带签名。
#
# 用法:
#   ./publish-updater.sh <发布目录> [仓库]
#
# 凭证（二选一，优先环境变量）:
#   ~/.config/updater/gh-token    需 repo 权限，chmod 600
#   GITHUB_TOKEN=xxx              临时覆盖用
#
# 示例:
#   ./publish-updater.sh ../pms-aichat/public/updater
#
set -euo pipefail

REPO="${2:-sunshine-lin/pluginUpdate_rust}"
ARTIFACT_NAME="updater-release"
BASE_URL="${BASE_URL:-https://chainai.cjdropshipping.cn/updater}"

PUBLISH_DIR="${1:-}"

die() { echo "[publish-updater] 错误: $*" >&2; exit 1; }
info() { echo "[publish-updater] $*"; }

[ -n "$PUBLISH_DIR" ] || die "用法: $0 <发布目录> [仓库]"
[ -d "$PUBLISH_DIR" ] || die "发布目录不存在: $PUBLISH_DIR"

# token 优先取环境变量，其次读凭证文件。放文件是为了让发布真正做到「一条命令」——
# 每次发版都要先 export 一遍，既容易忘，也容易在复制粘贴时泄露到别处
TOKEN_FILE="${GITHUB_TOKEN_FILE:-$HOME/.config/updater/gh-token}"
if [ -z "${GITHUB_TOKEN:-}" ] && [ -f "$TOKEN_FILE" ]; then
  # 去掉可能的换行/空白：粘贴时常带上，带进 HTTP 头会导致 401
  GITHUB_TOKEN="$(tr -d '[:space:]' < "$TOKEN_FILE")"
fi
[ -n "${GITHUB_TOKEN:-}" ] || die "缺少凭证。把 GitHub token（需 repo 权限）写入 $TOKEN_FILE，或设 GITHUB_TOKEN 环境变量"

command -v curl >/dev/null || die "缺少 curl"
command -v unzip >/dev/null || die "缺少 unzip"
command -v node >/dev/null || die "缺少 node（生成 latest.json 需要）"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN_SCRIPT="$SCRIPT_DIR/generate-latest-json.cjs"
[ -f "$GEN_SCRIPT" ] || die "找不到 $GEN_SCRIPT"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# ── 1. 取最新一次成功构建的 artifact ────────────────────────────
info "查询最新 $ARTIFACT_NAME ..."
ARTIFACT_JSON="$(curl -fsSL \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H "Accept: application/vnd.github+json" \
  "https://api.github.com/repos/$REPO/actions/artifacts?name=$ARTIFACT_NAME&per_page=1")" \
  || die "查询 artifact 失败（检查 GITHUB_TOKEN 与网络）"

DOWNLOAD_URL="$(printf '%s' "$ARTIFACT_JSON" \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const a=(JSON.parse(s).artifacts||[]).filter(x=>!x.expired);process.stdout.write(a.length?a[0].archive_download_url:"")})')"
[ -n "$DOWNLOAD_URL" ] || die "没有可用的 $ARTIFACT_NAME（可能已过期，需重新触发构建）"

info "下载 artifact ..."
curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
  -o "$WORK_DIR/artifact.zip" "$DOWNLOAD_URL" || die "下载 artifact 失败"

mkdir -p "$WORK_DIR/stage"
unzip -q "$WORK_DIR/artifact.zip" -d "$WORK_DIR/stage" || die "解压 artifact 失败"

SETUP_EXE="$(find "$WORK_DIR/stage" -maxdepth 1 -name '*-setup.exe' -print -quit)"
[ -n "$SETUP_EXE" ] || die "artifact 内没有 *-setup.exe，检查 CI 的 Collect signed installer 步骤"
info "取得安装包: $(basename "$SETUP_EXE")"

# 签名由 CI 在构建阶段产出，此处只校验其存在。缺签名说明 CI 没配私钥，
# 此时若照常发布，客户端会验签失败并静默拒绝更新——十几台会集体收不到
# 更新却查不出原因，故宁可在这里中止
[ -f "$SETUP_EXE.sig" ] \
  || die "artifact 内缺少 $(basename "$SETUP_EXE").sig；请确认 CI 已配置 TAURI_SIGNING_PRIVATE_KEY"

# ── 2. 生成 latest.json（复用已有测试覆盖的脚本，勿另写一份）──
VERSION="$(basename "$SETUP_EXE" | sed -E 's/.*_([0-9]+\.[0-9]+\.[0-9]+)_.*/\1/')"
[ -n "$VERSION" ] || die "无法从文件名解析版本号: $(basename "$SETUP_EXE")"
info "版本: $VERSION"

# 更新说明取自 CHANGELOG.md 对应版本段落——这段文字会显示给采购同事看，
# 写在仓库里可随代码一起评审，不必发布时现敲。取不到时回落为版本号本身，
# 只是界面上不显示说明，不影响升级
NOTES="$(node "$SCRIPT_DIR/extract-notes.cjs" "$SCRIPT_DIR/../CHANGELOG.md" "$VERSION")"

if [ -n "$NOTES" ]; then
  info "更新说明:"
  printf '%s\n' "$NOTES" | sed 's/^/    /'
else
  NOTES="aichat Updater $VERSION"
  info "CHANGELOG.md 中没有 $VERSION 的段落，界面将不显示更新说明"
fi

node "$GEN_SCRIPT" \
  "$WORK_DIR/stage" \
  "$VERSION" \
  "$BASE_URL" \
  "$WORK_DIR/stage/latest.json" \
  "$NOTES" >/dev/null || die "生成 latest.json 失败"

# ── 3. 原子落盘 ─────────────────────────────────────────────────
# 逐个 mv 而非先删后拷：客户端可能正好在这一刻拉取，
# 读到写了一半的 latest.json 会导致该次检查失败。
# 同目录内 mv 是原子替换，不存在中间态。
info "发布到 $PUBLISH_DIR ..."
for f in "$SETUP_EXE" "$SETUP_EXE.sig" "$WORK_DIR/stage/latest.json"; do
  name="$(basename "$f")"
  cp "$f" "$PUBLISH_DIR/.$name.tmp"
  mv -f "$PUBLISH_DIR/.$name.tmp" "$PUBLISH_DIR/$name"
done

info "完成。已发布:"
info "  $(basename "$SETUP_EXE")"
info "  $(basename "$SETUP_EXE").sig"
info "  latest.json (版本 $VERSION)"
echo
info "验证（注意看 content-type，不能只看状态码——"
info "该站点 nginx 配了 try_files 回落首页，不存在的路径也返 200+HTML）:"
info "  curl -sI $BASE_URL/latest.json | grep -i content-type   # 应为 application/json"
