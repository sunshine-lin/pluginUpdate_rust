# aichat-logs — 给一个插件实例名，就能拿到它的日志

给 Claude Code 用的技能。排查采购插件问题时，**不用知道那个实例在哪台机器、IP 是多少**：

```
「查一下 10-1-LS10005 的日志」
「虚拟机3-chrome7-cj07002 今天报了什么错」
「10 号机今天异常最多的是哪个实例」
```

## 安装

```bash
bash skills/aichat-logs/install.sh
```

装到 `~/.claude/skills/aichat-logs/`（**用户级**），所以在**任何项目**下都能用 ——
排查时日志在 `pluginUpdate_rust` 这边、插件代码在 `pms-aichat` 那边，
装成项目级就只有一个能用，等于逼人来回切目录。

## 前提

| | |
|---|---|
| Python | 3.8+（macOS 自带；Windows 需自行安装） |
| 网络 | **在公司局域网内**（含 VPN）。扫描范围 `192.168.0.x ~ 192.168.7.x` |
| 目标机器 | 客户端 ≥ 0.3.0（只读接口是那时加的），且已放行 17653 入站端口 |
| 凭证 | 不需要。**局域网内谁都能读** —— 这也意味着日志没有访问控制 |

## 它是怎么找到机器的

两层缓存，**都不被信任**：

| 缓存 | 会怎么失效 | 怎么自愈 |
|---|---|---|
| `machines.json`（机器→IP） | DHCP 换 IP | 回连校验对方自报的 `machineName`，对不上就扫网段 |
| `plugin-index.json`（插件→机器） | 实例增删/迁移 | 回连确认那台机器现在真有这个实例，没有就遍历重建 |

**为什么校验这一步不能省**：拿过期索引去查会连到一台**不存在该实例**的机器，
而返回的空结果和「这个实例今天没日志」**长得一模一样**，足以让人误判成机器挂了。
实测索引指错时 0.94 秒内自动重建并连对。

## 这两个缓存不随仓库分发

`machines.json` 存 10 台采购机的内网 IP 与主机名，`plugin-index.json` 存 90+ 个
实例名（含 1688/CJ 账号 ID）。两者都是**派生数据**，本机扫一次就能重建 ——
而本仓库的 GitHub 远端是**公开**的，提交上去等于把「哪些账号在跑自动化」告诉 1688。

同理，本目录下 `SKILL.md` / `aichat_logs.py` 里的示例账号 ID（`LS100xx` / `cj070xx`）
都是**占位符**，不是真实账号。真实实例名跑 `--list-plugins` 看本机索引。

## 常用命令

```bash
S=~/.claude/skills/aichat-logs/aichat_logs.py

python3 $S --plugin 10-1-LS10005 --date 2026-08-25 --level ERROR
python3 $S --plugin 虚拟机3-chrome7-cj07002 --summary
python3 $S --list-plugins        # 全部实例在哪台机器
python3 $S --scan                # 重扫机器（IP 变动后）
python3 $S --rebuild-index       # 重建实例索引（实例增删后）
```

## 数据敏感性

日志含**供应商聊天内容与采购报价**。token/密码/手机号已在写入前脱敏（插件侧
`sanitizeSensitiveInfo`），但**业务数据没脱** —— 脱了就没法排查。

日志**始终留在那台采购机本地**，远程模式只是读、不复制到别处。
不要把日志原文粘到外部服务，汇报时引用具体错误信息与统计结论即可。

## 两份副本

本目录同时存在于 `pluginUpdate_rust` 与 `pms-aichat`（方便从任一项目 pull）。
**改动请以 `pluginUpdate_rust` 为准**（日志接口就在这个项目里实现），
改完同步到 `pms-aichat`，避免两边漂移。
