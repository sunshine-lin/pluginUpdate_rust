#!/usr/bin/env bash
#
# 更新器发布脚本（在分发服务器上执行）
#
# 背景：十几台采购虚拟机的更新器不可能逐台手动替换。本脚本完成发布链路的
# 后半段——把 GitHub Actions 编译好的未签名安装包取回，在本机签名、生成
# latest.json，落到 nginx 网站目录，之后各机器自行拉取升级。
#
# 为什么签名在这里而不在 CI：签名是纯 minisign 对文件字节的运算，与操作系统
# 无关；而编译 NSIS 安装器必须在 Windows。两者分离后，私钥只需存在本服务器，
# 不必交给 GitHub Secret。
#
# 用法:
#   export GITHUB_TOKEN=ghp_xxx                       # 需 actions:read 权限
#   export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=xxx     # 私钥密码，无密码则不设
#   ./publish-updater.sh <私钥路径> <发布目录> [仓库]
#
# 示例:
#   ./publish-updater.sh /etc/aichat-updater/signing.key /usr/share/nginx/www/updater
#
set -euo pipefail

REPO="${3:-sunshine-lin/pluginUpdate_rust}"
ARTIFACT_NAME="updater-release"
BASE_URL="${BASE_URL:-https://chainai.cjdropshipping.cn/updater}"

PRIVATE_KEY_PATH="${1:-}"
PUBLISH_DIR="${2:-}"

die() { echo "[publish-updater] 错误: $*" >&2; exit 1; }
info() { echo "[publish-updater] $*"; }

[ -n "$PRIVATE_KEY_PATH" ] && [ -n "$PUBLISH_DIR" ] \
  || die "用法: $0 <私钥路径> <发布目录> [仓库]"
[ -f "$PRIVATE_KEY_PATH" ] || die "私钥文件不存在: $PRIVATE_KEY_PATH"
[ -d "$PUBLISH_DIR" ] || die "发布目录不存在: $PUBLISH_DIR"
[ -n "${GITHUB_TOKEN:-}" ] || die "需设置 GITHUB_TOKEN（actions:read 权限）"

command -v curl >/dev/null || die "缺少 curl"
command -v unzip >/dev/null || die "缺少 unzip"
command -v node >/dev/null || die "缺少 node（生成 latest.json 需要）"
command -v npx >/dev/null || die "缺少 npx（tauri signer 签名需要）"

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
[ -n "$SETUP_EXE" ] || die "artifact 内没有 *-setup.exe，检查 CI 的 Collect unsigned installer 步骤"
info "取得安装包: $(basename "$SETUP_EXE")"

# ── 2. 签名（产出同名 .sig，供下一步读取）─────────────────────
info "签名 ..."
npx tauri signer sign -f "$PRIVATE_KEY_PATH" "$SETUP_EXE" >/dev/null \
  || die "签名失败（私钥或 TAURI_SIGNING_PRIVATE_KEY_PASSWORD 不正确？）"
[ -f "$SETUP_EXE.sig" ] || die "签名后未生成 $SETUP_EXE.sig"

# ── 3. 生成 latest.json（复用已有测试覆盖的脚本，勿另写一份）──
VERSION="$(basename "$SETUP_EXE" | sed -E 's/.*_([0-9]+\.[0-9]+\.[0-9]+)_.*/\1/')"
[ -n "$VERSION" ] || die "无法从文件名解析版本号: $(basename "$SETUP_EXE")"
info "版本: $VERSION"

node "$GEN_SCRIPT" \
  "$WORK_DIR/stage" \
  "$VERSION" \
  "$BASE_URL" \
  "$WORK_DIR/stage/latest.json" \
  "aichat Updater $VERSION" >/dev/null || die "生成 latest.json 失败"

# ── 4. 原子落盘 ─────────────────────────────────────────────────
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
