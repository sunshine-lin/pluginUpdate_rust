---
name: aichat-logs
description: 查 AIChat 插件（CJ 采购插件，装在 10 台采购虚拟机上、共 90+ 个 Chrome 实例）的运行时日志。**给一个插件实例名就能拿到日志**——如 10-1-LS10005、虚拟机3-chrome7-cj07002、8-14-LS10002、7-11-LS10003、虚拟机5-chrome1-LS10001，工具自动定位它在哪台机器、IP 是多少，使用者不必知道。也可给机器名（DESKTOP-MQUBUQS-2de5364f 这类）或不给参数查本机。用于排查：某个实例为什么卡住/不跑任务/报错、WS 断连、1688 登出或掉登录、账号不对、抓取失败、滑块验证失败、任务认领冲突、客户端自更新失败。任何提到"某某实例/某台采购机/某个 chrome 有问题、看下日志、查下报错、为什么没跑"的场景都适用，本技能可在任意项目目录下使用（用户级技能）。只解决【看插件运行时日志】；看插件代码实现走 codegraph、查业务规则走 business-kb、查线上服务端日志走 sls-query。先 --summary 看全局再收窄，输出为 JSON。
---

# aichat-logs：AIChat 插件日志查询（本机 / 局域网远程）

排查采购插件问题时按【日志】定位：哪个实例在报错、报什么错、什么时候开始的。

**核心用法：给一个机器名（如 `10-2-cj070014`）就能拿到那台采购虚拟机的日志。**
不必知道 IP——技能自动解析（清单 + 扫网段兜底），日志始终落盘在那台机器本地、
不复制到别处。

**用途边界（先过意图门）**：本技能只解决「看插件运行时日志」。看插件代码实现→codegraph；
查业务规则/接口→business-kb；查线上服务端（k8s/nginx）日志→sls-query。
日志里找到线索后，再决定要不要翻码。

## ⚠️ 数据敏感性

日志含**供应商聊天内容与采购报价**。token/密码/手机号已在写入前脱敏
（插件侧 `sanitizeSensitiveInfo`），但**业务数据没脱**——脱了就没法排查。

已与开发确认：只用公司内部 AI、**数据不出内网**。不要把日志原文粘贴到外部服务，
也不要在汇报里大段引用聊天内容——引用具体错误信息与统计结论即可。

## 用法

```bash
python3 ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/aichat-logs/aichat_logs.py [选项]
```

### 给插件实例名（最常用，也是本技能的核心）

**使用者记得住的是插件名，不是机器名。** 直接给 `--plugin`、不给 `--machine`，
工具会自动定位它在哪台机器：

```bash
aichat_logs.py --plugin 10-1-LS10005 --date 2026-08-25 --level ERROR
aichat_logs.py --plugin 虚拟机3-chrome7-cj07002 --summary
```

会在 stderr 打一行 `10-1-LS10005 位于 DESKTOP-MQUBUQS-2de5364f (192.168.7.16)`，
正文 JSON 里带 `_ip`。

**为什么这层映射非做不可**：机器名是 Windows 主机名 + 随机后缀
（`DESKTOP-MQUBUQS-2de5364f`），跟插件的编号体系完全对不上 —— 插件叫
「虚拟机2-chrome1-cj07003」的那台机器名是 `WIN-G9I86IADRC1-63316932`，而
「虚拟机3」在**同主机名**的另一台上（同镜像克隆，只有随机后缀能区分）。

**实例命名有三种风格**，都支持：
`虚拟机N-chromeM-账号`（如 虚拟机5-chrome1-LS10001）、
`N-M-账号`（如 10-1-LS10005、8-14-LS10002）、
`虚拟机N-M-账号`（如 虚拟机7-3-LS10004）。账号有 `LS*` 与 `cj*` 两类。

```bash
aichat_logs.py --list-plugins     # 看 插件名→机器 索引（全部 90+ 个实例）
aichat_logs.py --rebuild-index    # 遍历所有机器重建索引（实例增删后用）
```

### 给机器名

```bash
aichat_logs.py --machine DESKTOP-MQUBUQS-2de5364f --summary
aichat_logs.py --machine DESKTOP-MQUBUQS-2de5364f --level ERROR --limit 50

aichat_logs.py --list-machines    # 看当前已知的机器
aichat_logs.py --scan             # 扫网段刷新清单（约 40 秒）
```

### 两层缓存都不被信任

`machines.json`（机器→IP）与 `plugin-index.json`（插件→机器）**各自校验各自的
失效方式**：前者回连校验对方自报的 `machineName`（DHCP 换 IP 时自愈），
后者回连确认那台机器现在真有这个实例（实例增删/迁移时自愈）。

拿过期索引去查会连到一台**不存在该实例**的机器，而返回的空结果和
「这个实例今天没日志」**长得一模一样**，足以让人误判成机器挂了 ——
所以校验这一步不能省。实测索引指错时 0.94 秒内自动重建并连对。

**机器名 → IP 是怎么解决的**：清单文件 `machines.json` 记录映射，但**不依赖它准确**
——连上后会校验对方返回的 `machineName`，对不上或连不上就自动扫网段重找并更新清单。
所以 IP 变了不用管，第一次查会慢一点（要扫描）。

**机器名就是插件的下载目录名**（每台机器不同且稳定，团队本来就用它指代机器）。
不确定有哪些机器时先跑 `--scan`。

不带 `--machine` 则查本机（在装了客户端的机器上直接用）。

### 建议顺序：先看全局，再收窄

**一天可达几十万行（15 个实例共用一份日志），不要一上来就拉明细。**

```bash
# 第一步：探活——确认有哪些日期的日志（只保留 7 天）
aichat_logs.py --dates

# 第二步：看全局——哪个实例异常最多、主要是哪几类错误
aichat_logs.py --summary --date 2026-08-22

# 第三步：按上一步的结论收窄，查明细
aichat_logs.py --date 2026-08-22 --plugin robot-03 --level ERROR --limit 50
aichat_logs.py --date 2026-08-22 --keyword 超时 --from 09:00 --to 12:00
```

### 选项

| 选项 | 说明 |
|---|---|
| `--dates` | 列出有日志的日期 |
| `--summary` | 聚合概览（见下节），**扫全量**，不受分页上限约束 |
| `--date <YYYY-MM-DD>` | 日期，默认今天 |
| `--level <A,B>` | 级别筛选，逗号分隔，如 `ERROR,WARN` |
| `--plugin <A,B>` | 实例名筛选（如 `robot-03`），逗号分隔 |
| `--keyword <词>` | 大小写不敏感，匹配 消息+来源+级别+实例名 |
| `--from` / `--to` | 时间段，`HH:MM`，**北京时间** |
| `--error-only` | 只读异常归集文件（`aichat-error-*.log`） |
| `--offset` / `--limit` | 分页，limit 默认 500、上限 2000 |

## `--summary` 输出怎么读

```json
{
  "machineId": "WIN-BUYER01-a1b2c3d4",
  "summary": {
    "total": 400082,
    "byLevel": { "ERROR": 40316, "WARN": 39780, "INFO": 280008 },
    "byPlugin": [ { "pluginName": "robot-03", "errors": 5459, "total": 26913 } ],
    "topErrors": [
      { "count": 10186, "plugins": ["robot-01", "..."], "pattern": "抓取失败 #<N>",
        "firstSeen": "...", "lastSeen": "...", "sample": "抓取失败 #12345" }
    ]
  }
}
```

- **`byPlugin` 已按异常数倒序**——第一个就是最该查的实例
- **`topErrors[].plugins` 是判断问题性质的关键**：
  - `plugins` 有 10+ 个实例 → **共性问题**（1688 改版、网络、插件 bug），别只盯一台
  - `plugins` 只有 1~2 个 → **该实例自己的问题**（登出、账号异常、卡死）
- **`pattern` 里的 `<N>` / `<URL>` 是归一占位符**：同类错误的可变部分（ID、数量、URL）
  被替换后归并，所以 `count` 是「这类错误」的总数，不是某条具体消息的数量。
  要看真实消息看 `sample`

## ⚠️ `total: 0` 不是结论

查不到数据时**先自证**，不要直接回「没有异常/没问题」：

1. **日期对不对** —— 先 `--dates` 看有哪些日期。**日志只保留 7 天**，查更早的必然为空，
   这不代表当时没问题
2. **是不是筛太窄** —— 去掉 `--plugin` / `--keyword` 再看，或先跑 `--summary`
3. **实例名对不对** —— 实例名是插件自报的下载目录名（如 `robot-03`），
   `--summary` 的 `byPlugin` 里有当天全部实例名，照那个写
4. **时间段** —— `--from/--to` 是**北京时间**的 `HH:MM`

上面都排除后仍为 0，才能说「该条件下无记录」。

## ⚠️ 时间戳的坑

日志文件里的时间戳**已归一为北京时间**（客户端 `normalize_timestamp` 处理过）。
但**插件侧上报的原始时间是 UTC** —— 如果看到某个现象的时间点整体差 8 小时，
先怀疑这条归一链路，而不是去猜业务逻辑。

## 常见排查路径

**「某台机器的任务不跑了」**
```bash
aichat_logs.py --summary                          # 先看今天哪个实例异常最多
aichat_logs.py --plugin robot-XX --level ERROR,WARN --limit 100   # 再看那个实例
```
配合客户端的「插件巡检」页看实时状态（WS 是否连接、1688 是否登出、侧边栏是否打开）——
日志说「过去发生了什么」，巡检页说「现在什么状态」，两者结合才完整。

**「所有实例好像都有问题」**
看 `--summary` 的 `topErrors[].plugins`：如果一类错误覆盖 10+ 实例，
那是共性问题（1688 页面改版、网络、插件版本 bug），去查那类错误的 `sample` 与 `firstSeen`
——`firstSeen` 往往能对上某次发版或 1688 改版的时间点。

**「只想看崩溃」**
`--error-only` 读的是异常单独归集的文件，比在全量里筛更快。

## 说明

- 输出为 JSON。`{"error": ...}` 表示查询失败，如实告知，不要假装查到了
- **日志不出那台机器**：远程模式只是读，日志始终落盘在采购虚拟机本地
- `machineName` 标识数据来自哪台机器；`pluginName` 区分同机的多个实例
  （一台机器跑 3~15 个 Chrome 实例，各自独立下载目录）

## ⚠️ 连不上目标机器时

报错里会带排查提示，按顺序确认：

1. **那台机器开着吗**、客户端在跑吗
2. **防火墙放行了吗** —— Windows 默认拦截入站，需要在那台机器上执行一次：
   ```cmd
   netsh advfirewall firewall add rule name="aichat-updater" dir=in action=allow protocol=TCP localport=17653
   ```
   这是目前**唯一需要在每台机器上各做一次**的操作
3. **客户端版本够新吗** —— 只读接口是 2026-08-22 加的，旧版没有 `/api/*`
4. **机器名写对了吗** —— 跑 `--scan` 看局域网里实际有哪些
