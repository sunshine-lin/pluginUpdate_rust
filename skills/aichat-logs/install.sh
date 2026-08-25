#!/usr/bin/env bash
# 把 aichat-logs 技能装到本机的 Claude Code 用户级技能目录。
#
# 装完在**任何项目**下跟 CC 说「查一下 <插件实例名> 的日志」即可，
# 不需要知道那个实例在哪台机器、IP 是多少。
#
# 用法：  bash install.sh
#
# # 为什么装到用户级（~/.claude/skills/）而不是项目级
# 排查插件问题时，日志在这边、插件代码在 pms-aichat 那边。装成项目级就只有
# 一个项目能用，等于逼人来回切目录。用户级则两边都能用。

set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/aichat-logs"

echo "安装 aichat-logs 技能"
echo "  从: $SRC"
echo "  到: $DEST"

if ! command -v python3 >/dev/null 2>&1; then
  echo "错误: 找不到 python3。macOS 自带；Windows 需要先装 Python 3.8+。" >&2
  exit 2
fi

mkdir -p "$DEST"
cp "$SRC/SKILL.md" "$SRC/aichat_logs.py" "$DEST/"
chmod +x "$DEST/aichat_logs.py"
echo "已复制 SKILL.md 与 aichat_logs.py"

# 两个缓存文件**故意不随仓库分发**：
# - machines.json 存 10 台采购机的内网 IP 与主机名
# - plugin-index.json 存 90+ 个实例名（含 1688/CJ 账号 ID）
# 它们都是派生数据，本机扫一次就能重建；而 pluginUpdate_rust 的 GitHub 仓库
# 是**公开**的，把这些提交上去等于把「哪些账号在跑自动化」告诉 1688。
echo
echo "首次使用需要建两份本机缓存（都是派生数据，不随仓库分发）："
echo "  1) 扫描局域网找机器（约 40 秒）"
python3 "$DEST/aichat_logs.py" --scan >/dev/null 2>&1 && echo "     完成" || {
  echo "     ⚠️ 扫描没找到机器。确认你在公司局域网内（含 VPN），稍后可手工重跑：" >&2
  echo "        python3 $DEST/aichat_logs.py --scan" >&2
}
echo "  2) 建立 插件名→机器 索引（约 30 秒）"
python3 "$DEST/aichat_logs.py" --rebuild-index >/dev/null 2>&1 && echo "     完成" || {
  echo "     ⚠️ 索引未建立，可稍后重跑：" >&2
  echo "        python3 $DEST/aichat_logs.py --rebuild-index" >&2
}

echo
echo "装好了。在任意项目下跟 CC 说："
echo "  「查一下 10-1-LS10005 的日志」"
echo "  「虚拟机3-chrome7-cj07002 今天报了什么错」"
echo "  「10 号机今天异常最多的是哪个实例」"
echo
echo "也可以直接跑："
echo "  python3 $DEST/aichat_logs.py --list-plugins    # 看全部实例在哪台机器"
