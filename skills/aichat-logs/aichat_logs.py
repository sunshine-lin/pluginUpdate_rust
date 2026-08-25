#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
aichat_logs.py — AIChat 插件日志查询（DEV-125123 / DEV-125226）

两种模式，自动选择：
- **远程**：给机器名（如 10-2-cj070014），从局域网连那台采购虚拟机读日志
- **本地**：不给机器名时，读本机客户端的日志（在装了客户端的机器上直接用）

# 机器名 → IP 怎么解决
清单文件 machines.json 记录映射，但**不依赖它准确**：连上后校验对方返回的
machineName，对不上或连不上就扫网段重找并自动更新清单。虚拟机 IP 是否固定
尚未确认，此设计对固定 IP 与 DHCP 两种情况都成立。

用法:
  aichat_logs.py --machine 10-2-cj070014 --summary
  aichat_logs.py --machine 10-2-cj070014 --level ERROR --limit 50
  aichat_logs.py --scan                    # 扫网段，生成/刷新机器清单
  aichat_logs.py --list-machines           # 看当前清单
  aichat_logs.py --summary                 # 不带 --machine 则查本机
"""
import os
import sys
import json
import shutil
import argparse
from typing import Dict, List, Optional, Tuple
import subprocess
import urllib.request
import urllib.error
import urllib.parse
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
MACHINES_FILE = os.path.join(HERE, "machines.json")
# 插件名 → 机器名。与 machines.json 分开：前者是"实例在哪台机器"（会随实例增删变），
# 后者是"机器在哪个 IP"（会随 DHCP 变），两种失效方式不同，混在一起不好各自校验
PLUGIN_INDEX_FILE = os.path.join(HERE, "plugin-index.json")

PORT = 17653
SUBCOMMAND = "query-logs"

# 扫描网段。公司网络实测掩码 255.255.248.0（192.168.0.x ~ 192.168.7.x）
DEFAULT_SCAN_PREFIXES = [f"192.168.{i}." for i in range(8)]
# 单个地址的连接超时。局域网内可达的机器响应在毫秒级，
# 给 0.6 秒足够；再大会让全网段扫描慢得没法用（2048 个地址）
PROBE_TIMEOUT = 0.6
QUERY_TIMEOUT = 30


# ───────────────────────── 机器清单 ─────────────────────────


def load_machines():
    try:
        with open(MACHINES_FILE, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def save_machines(m):
    try:
        with open(MACHINES_FILE, "w", encoding="utf-8") as f:
            json.dump(m, f, ensure_ascii=False, indent=2, sort_keys=True)
    except OSError:
        # 清单写不进去不影响本次查询，只是下次要重新扫
        pass


# ───────────────────────── 远程访问 ─────────────────────────


def http_get(ip, path, timeout):
    """GET 一个客户端接口。任何失败都返回 None，不抛异常"""
    url = f"http://{ip}:{PORT}{path}"
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except Exception:
        return None


def probe(ip):
    """探测一个地址是否跑着客户端。是则返回 (机器名, ip)"""
    data = http_get(ip, "/api/log-dates", PROBE_TIMEOUT)
    if not data:
        return None
    name = data.get("machineName")
    return (name, ip) if name else None


def scan_network(prefixes=None):
    """扫网段找所有跑着客户端的机器。返回 {机器名: ip}"""
    prefixes = prefixes or DEFAULT_SCAN_PREFIXES
    targets = [f"{p}{i}" for p in prefixes for i in range(1, 255)]
    found = {}
    # 并发扫描：2000 个地址串行要跑 20 分钟，并发后约 10 秒
    with ThreadPoolExecutor(max_workers=100) as pool:
        for result in pool.map(probe, targets):
            if result:
                name, ip = result
                found[name] = ip
                print(f"  发现 {name} @ {ip}", file=sys.stderr)
    return found


def resolve_machine(name):
    """
    把机器名解析成 IP。

    先查清单并**校验对方确实是这台机器**——IP 可能已经变了、或被别的机器
    占用。校验不过就扫网段重找并更新清单。这一步是「给个机器名就能拿到
    日志」的关键：使用者不必知道 IP，也不必在 IP 变化时手工维护清单。
    """
    machines = load_machines()
    cached = machines.get(name)
    if cached:
        data = http_get(cached, "/api/log-dates", PROBE_TIMEOUT)
        if data and data.get("machineName") == name:
            return cached
        print(f"清单里的 {cached} 已不是 {name}，重新扫描…", file=sys.stderr)

    print(f"扫描局域网查找 {name}…", file=sys.stderr)
    found = scan_network()
    if found:
        machines.update(found)
        save_machines(machines)
    return found.get(name)


# ──────────────────── 插件名 → 机器 ────────────────────
#
# 使用者记得住的是插件名（"10-1-LS10005 出问题了"），不是机器名——
# 机器名是 Windows 主机名 + 随机后缀（如 DESKTOP-MQUBUQS-2de5364f），
# 跟插件的编号体系完全对不上：插件叫"虚拟机2-chrome1-cj07003"的那台，
# 机器名是 WIN-G9I86IADRC1-63316932，而"虚拟机3"在**同主机名**的另一台上
# （同镜像克隆，只有随机后缀能区分）。这层映射不做进工具，就只能存在人脑里。


def load_plugin_index():
    try:
        with open(PLUGIN_INDEX_FILE, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def save_plugin_index(idx):
    try:
        with open(PLUGIN_INDEX_FILE, "w", encoding="utf-8") as f:
            json.dump(idx, f, ensure_ascii=False, indent=2, sort_keys=True)
    except OSError:
        pass


def list_plugins_on(ip, date=None):
    """取某台机器上有日志的插件名。取不到返回空列表（不抛异常）"""
    q = f"?limit=1&date={date}" if date else "?limit=1"
    data = http_get(ip, "/api/log-entries" + q, QUERY_TIMEOUT)
    if not data:
        return []
    names = data.get("plugin_names") or data.get("pluginNames") or []
    return [n for n in names if n not in ("client", "unknown")]


def resolve_machine_by_plugin(plugin):
    """
    把插件名解析成机器名。

    先查索引并**回连那台机器确认它现在确实有这个插件**——索引只是加速的线索，
    不是真相。实例会增删、会迁到别的机器，拿过期索引去查会指向一台不存在
    该实例的机器，而返回的空结果看起来和"这个实例今天没日志"一模一样，
    足以让人误判成机器挂了。

    确认不过就遍历已知机器重建索引。返回 (机器名, IP)，找不到返回 (None, None)。
    """
    idx = load_plugin_index()
    cached = idx.get(plugin)
    if cached:
        ip = resolve_machine(cached)
        if ip and plugin in list_plugins_on(ip):
            return cached, ip
        print(f"索引里的 {cached} 上已找不到 {plugin}，重建索引…", file=sys.stderr)

    machines = load_machines()
    if not machines:
        machines = scan_network()
        save_machines(machines)

    hit = None
    rebuilt = {}
    for name, ip in sorted(machines.items()):
        for p in list_plugins_on(ip):
            rebuilt[p] = name
            if p == plugin:
                hit = (name, ip)
    if rebuilt:
        idx.update(rebuilt)
        save_plugin_index(idx)
    return hit if hit else (None, None)


# ───────────────────────── 本地回落 ─────────────────────────

CANDIDATES_MACOS = [
    "/Applications/aichat Updater.app/Contents/MacOS/aichat-updater",
    "/Applications/aichat Updater Test.app/Contents/MacOS/aichat-updater",
    os.path.expanduser(
        "~/cj/pluginUpdate_rust/src-tauri/target/release/bundle/macos/"
        "aichat Updater Test.app/Contents/MacOS/aichat-updater"
    ),
    os.path.expanduser("~/cj/pluginUpdate_rust/src-tauri/target/release/aichat-updater"),
]
CANDIDATES_WINDOWS = [
    r"C:\Program Files\aichat Updater\aichat-updater.exe",
    os.path.expandvars(r"%LOCALAPPDATA%\Programs\aichat Updater\aichat-updater.exe"),
]


def find_binary():
    override = os.environ.get("AICHAT_UPDATER_BIN")
    if override and os.path.isfile(override):
        return override
    candidates = CANDIDATES_WINDOWS if sys.platform == "win32" else CANDIDATES_MACOS
    existing = [p for p in candidates if os.path.isfile(p)]
    if existing:
        # 取最新的：正式安装位置的版本可能旧、不认 query-logs 子命令，
        # 而旧版会把参数当成「唤回窗口」拉起 GUI 挂住（已实测踩到）
        return max(existing, key=os.path.getmtime)
    return shutil.which("aichat-updater")


def query_local(args):
    binary = find_binary()
    if not binary:
        print(json.dumps({"error": "本机找不到 aichat-updater 客户端"}, ensure_ascii=False))
        return 2
    try:
        proc = subprocess.run(
            [binary, SUBCOMMAND] + args, capture_output=True, text=True, timeout=30
        )
    except subprocess.TimeoutExpired:
        print(
            json.dumps(
                {"error": "查询超时，最常见原因是客户端版本过旧、不支持 query-logs"},
                ensure_ascii=False,
            )
        )
        return 2
    sys.stdout.write(proc.stdout)
    if proc.stderr.strip():
        sys.stderr.write(proc.stderr)
    return proc.returncode


# ───────────────────────── 主流程 ─────────────────────────


def build_query(a):
    """把命令行参数拼成远程接口的查询串"""
    params = []
    if a.date:
        params.append(f"date={a.date}")
    if a.level:
        params.append(f"levels={urllib.parse.quote(a.level)}")
    if a.plugin:
        params.append(f"pluginNames={urllib.parse.quote(a.plugin)}")
    if a.keyword:
        params.append(f"keyword={urllib.parse.quote(a.keyword)}")
    if getattr(a, "from_time", None):
        params.append(f"startTime={urllib.parse.quote(a.from_time)}")
    if a.to:
        params.append(f"endTime={urllib.parse.quote(a.to)}")
    if a.offset:
        params.append(f"offset={a.offset}")
    if a.limit:
        params.append(f"limit={a.limit}")
    return "?" + "&".join(params) if params else ""


def main():
    p = argparse.ArgumentParser(add_help=True)
    p.add_argument("--machine", help="目标机器名（插件下载目录名，如 10-2-cj070014）")
    p.add_argument("--scan", action="store_true", help="扫网段刷新机器清单")
    p.add_argument("--list-machines", action="store_true", help="显示当前清单")
    p.add_argument("--list-plugins", action="store_true", help="显示 插件名→机器 索引")
    p.add_argument("--rebuild-index", action="store_true", help="遍历所有机器重建插件索引")
    p.add_argument("--dates", action="store_true", help="列出有日志的日期")
    p.add_argument("--summary", action="store_true", help="聚合概览")
    p.add_argument("--date")
    p.add_argument("--level")
    p.add_argument("--plugin")
    p.add_argument("--keyword")
    p.add_argument("--from", dest="from_time")
    p.add_argument("--to")
    p.add_argument("--offset", type=int)
    p.add_argument("--limit", type=int)
    a = p.parse_args()

    if a.list_machines:
        print(json.dumps(load_machines(), ensure_ascii=False, indent=2))
        return 0

    if a.scan:
        print("扫描局域网…", file=sys.stderr)
        found = scan_network()
        machines = load_machines()
        machines.update(found)
        save_machines(machines)
        print(json.dumps({"found": len(found), "machines": found}, ensure_ascii=False, indent=2))
        return 0

    if a.list_plugins:
        print(json.dumps(load_plugin_index(), ensure_ascii=False, indent=2))
        return 0

    if a.rebuild_index:
        machines = load_machines()
        if not machines:
            machines = scan_network()
            save_machines(machines)
        idx = {}
        for name, ip in sorted(machines.items()):
            for p in list_plugins_on(ip):
                idx[p] = name
            print(f"  {name} ({ip})", file=sys.stderr)
        save_plugin_index(idx)
        print(json.dumps({"plugins": len(idx), "index": idx}, ensure_ascii=False, indent=2))
        return 0

    # 只给插件名、没给机器名 → 自动定位它在哪台机器。
    # 这是本技能的核心用法：使用者记得住的是插件名，不是机器名
    if a.plugin and not a.machine:
        first = a.plugin.split(",")[0].strip()
        machine, ip = resolve_machine_by_plugin(first)
        if not machine:
            print(json.dumps({
                "error": f"局域网内没有任何机器上有插件 {first}",
                "hint": "确认插件名拼写正确（可用 --rebuild-index 重建索引看全部实例名）；"
                        "也可能那台机器关机、或防火墙没放行 17653 端口",
            }, ensure_ascii=False, indent=2))
            return 2
        print(f"{first} 位于 {machine} ({ip})", file=sys.stderr)
        a.machine = machine

    # 不指定机器名 → 查本机（在装了客户端的机器上直接用）
    if not a.machine:
        local_args = []
        for flag, val in [
            ("--date", a.date), ("--level", a.level), ("--plugin", a.plugin),
            ("--keyword", a.keyword), ("--from", a.from_time), ("--to", a.to),
        ]:
            if val:
                local_args += [flag, val]
        for flag, val in [("--offset", a.offset), ("--limit", a.limit)]:
            if val:
                local_args += [flag, str(val)]
        if a.dates:
            local_args.append("--dates")
        if a.summary:
            local_args.append("--summary")
        return query_local(local_args)

    # 远程查询
    ip = resolve_machine(a.machine)
    if not ip:
        print(
            json.dumps(
                {
                    "error": f"局域网内找不到机器 {a.machine}",
                    "hint": "确认该机器已开机、装了客户端、且防火墙放行了 17653 端口："
                    'netsh advfirewall firewall add rule name="aichat-updater" '
                    "dir=in action=allow protocol=TCP localport=17653",
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 2

    path = "/api/log-dates" if a.dates else (
        "/api/log-summary" if a.summary else "/api/log-entries"
    )
    data = http_get(ip, path + build_query(a), QUERY_TIMEOUT)
    if data is None:
        print(json.dumps({"error": f"连接 {a.machine} ({ip}) 失败"}, ensure_ascii=False))
        return 2
    data["_ip"] = ip
    print(json.dumps(data, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
