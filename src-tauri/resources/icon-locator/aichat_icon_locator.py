# -*- coding: utf-8 -*-
"""
识别 Chrome 工具栏上「AIChat 扩展图标」的屏幕坐标。

原始版本由领导提供（用于 ChatGPT 图标 + ShadowBot 场景），此处除以下几处外
**未改动任何逻辑**：
  1. 默认模板换成 aichat_template.png（原来是 ChatGPT 的花瓣 logo，
     拿它去找我们的蓝色文档+放大镜图标必然 NOT_FOUND）
  2. 文件更名，避免下一个人被 chatgpt 这个名字误导
  3. DEV-125986：新增 --region/--hwnd，支持把匹配范围收窄到指定窗口，
     不再永远全屏扫描（见下方「多实例定位」说明）

多实例定位（DEV-125986，2026-08-28 真机验证方案落地）：
原方案在整个虚拟桌面上取匹配分数最高的**一个**图标，一台采购机开着
8~10 个 Chrome 窗口、每个都有同样的图标时，返回的是「最像模板的那个」
而非「你要的那个」。客户端侧现在已经能通过 UI Automation（单进程多
Profile 架构）或映射表+进程枚举（独立进程架构）精确定位到目标窗口的
HWND，本脚本新增 --region 参数接收该窗口的屏幕矩形（客户端侧
GetWindowRect 得到），把截图和模板匹配范围都收窄到这个矩形内——
不传 --region 时保持原有全屏扫描行为（向后兼容，未接入新定位方案的
调用方不受影响）。

--hwnd 是配套的精确激活参数：--activate-match 按标题子串匹配、多个
同标题窗口时无法区分该抬哪一个（原有的"标题最长"启发式对多实例场景
基本无效）；传入 --hwnd 后直接激活这一个已知句柄，不再依赖标题匹配。
两者互斥：给了 --hwnd 就忽略 --activate-match。

方法：整屏（或 --region 限定的区域）截图 + OpenCV 多尺度模板匹配 ——
对分辨率 / DPI 缩放 / 窗口位置免疫。
给 ShadowBot 用：默认在标准输出打印一行 `x,y`（物理像素、绝对屏幕坐标），
找不到则退出码=2，出错退出码=1。

依赖：opencv-python(cv2)、numpy、Pillow。不是 Python 版本硬性要求（脚本
只用了各版本长期稳定支持的常规语法/API），3.8 及以上均可；"3.10"最初只是
照抄领导原始环境的版本号，10 号机实测装 3.12 同样能正常运行。目标机器若
没装这三个库，需要 `pip install opencv-python numpy Pillow`。

用法示例：
  python aichat_icon_locator.py                 # 打印 "1678,62"（全屏扫描，旧行为）
  python aichat_icon_locator.py --json          # 打印 JSON(含置信度/尺度)
  python aichat_icon_locator.py --click         # 直接移动鼠标并左键点击
  python aichat_icon_locator.py --save-debug d:\\tmp\\hit.png   # 存一张带命中框的图核对
  python aichat_icon_locator.py --threshold 0.55 --template 别的图标.png
  python aichat_icon_locator.py --hwnd 197304 --activate --region 100,50,300,80 --click
      # 多实例场景：精确激活+限定区域匹配+点击目标实例的图标
"""
import argparse
import ctypes
import json
import os
import sys
import time

import cv2
import numpy as np
from PIL import ImageGrab

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_TEMPLATE = os.path.join(HERE, "aichat_template.png")


def imread_unicode(path, flags=cv2.IMREAD_COLOR):
    """读图，兼容中文/非 ASCII 路径。

    cv2.imread 底层走的是 ASCII 文件 API，路径含中文时**静默返回 None**
    （不抛异常），表现为「模板读取失败」而看不出真正原因。
    改成自己读字节再交给 cv2.imdecode，绕开文件名这一层。
    """
    try:
        buf = np.fromfile(path, dtype=np.uint8)
        return cv2.imdecode(buf, flags)
    except Exception:
        return None


def imwrite_unicode(path, img):
    """写图，兼容中文/非 ASCII 路径。cv2.imwrite 有同样的问题——
    它返回 False 而不抛错，于是「debug 图没生成」这件事毫无线索。"""
    try:
        ext = os.path.splitext(path)[1] or ".png"
        ok, buf = cv2.imencode(ext, img)
        if not ok:
            return False
        buf.tofile(path)
        return True
    except Exception:
        return False


def make_dpi_aware():
    """让本进程按物理像素工作，截图和鼠标坐标才和 ShadowBot 对得上。"""
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)  # PER_MONITOR_DPI_AWARE
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass


def virtual_origin():
    """虚拟桌面左上角在绝对坐标里的偏移（多屏且副屏在左/上时为负）。"""
    u = ctypes.windll.user32
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN = 76, 77
    return u.GetSystemMetrics(SM_XVIRTUALSCREEN), u.GetSystemMetrics(SM_YVIRTUALSCREEN)


def parse_region(s):
    """解析 --region 参数值："left,top,width,height"（虚拟桌面绝对坐标，
    与 GetWindowRect 同一坐标系）。格式错误时抛 ValueError，由调用方
    统一处理成参数错误退出，而不是让 argparse 之外的地方悄悄吞掉。"""
    parts = s.split(",")
    if len(parts) != 4:
        raise ValueError("region 必须是 left,top,width,height 四个数字")
    left, top, width, height = (int(p.strip()) for p in parts)
    if width <= 0 or height <= 0:
        raise ValueError("region 的 width/height 必须为正数")
    return left, top, width, height


def grab_screen_bgr(region=None):
    """截图。region 为 (left, top, width, height)，是虚拟桌面绝对坐标
    （与 GetWindowRect 同一坐标系）；不传则截整个虚拟桌面（原有行为）。

    PIL 的 bbox 参数用的是"相对主屏幕左上角"的坐标系，而客户端传来的
    region 是虚拟桌面绝对坐标——多屏且副屏在左/上时两者有偏移，必须先
    减去 virtual_origin() 才能对齐，否则副屏上的窗口会截到错误位置。
    """
    if region is None:
        img = ImageGrab.grab(all_screens=True)
    else:
        left, top, width, height = region
        vx, vy = virtual_origin()
        bbox = (left - vx, top - vy, left - vx + width, top - vy + height)
        img = ImageGrab.grab(bbox=bbox, all_screens=True)
    return cv2.cvtColor(np.array(img), cv2.COLOR_RGB2BGR)


def _enum_windows():
    """返回 [(hwnd, title)]，仅可见且有标题的顶层窗口。"""
    u = ctypes.windll.user32
    u.IsWindowVisible.argtypes = [ctypes.c_void_p]
    u.GetWindowTextLengthW.argtypes = [ctypes.c_void_p]
    u.GetWindowTextW.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_int]
    WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    out = []

    def _cb(hwnd, _lparam):
        if not u.IsWindowVisible(hwnd):
            return True
        n = u.GetWindowTextLengthW(hwnd)
        if n <= 0:
            return True
        buf = ctypes.create_unicode_buffer(n + 1)
        u.GetWindowTextW(hwnd, buf, n + 1)
        out.append((hwnd, buf.value))
        return True

    u.EnumWindows(WNDENUMPROC(_cb), 0)
    return out


def _prep_window_api(u):
    for f in ("IsIconic", "ShowWindow", "SwitchToThisWindow",
              "BringWindowToTop", "SetForegroundWindow"):
        getattr(u, f).argtypes = [ctypes.c_void_p] + \
            ([ctypes.c_int] if f == "ShowWindow" else
             [ctypes.c_bool] if f == "SwitchToThisWindow" else [])


def _activate_hwnd(u, hwnd, wait_ms):
    """把指定句柄的窗口抬到最前（越过前台锁）。"""
    if u.IsIconic(hwnd):
        u.ShowWindow(hwnd, 9)  # SW_RESTORE
    # 轻敲 ALT 解除前台锁，再抬窗
    VK_MENU, KEYEVENTF_KEYUP = 0x12, 0x0002
    u.keybd_event(VK_MENU, 0, 0, 0)
    u.keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0)
    try:
        u.SwitchToThisWindow(hwnd, True)
    except Exception:
        pass
    u.BringWindowToTop(hwnd)
    u.SetForegroundWindow(hwnd)
    time.sleep(max(0, wait_ms) / 1000.0)


def activate_window_by_hwnd(hwnd, wait_ms=500):
    """精确激活指定 HWND（DEV-125986：客户端已知道目标窗口句柄时用这个，
    不再依赖标题匹配——多实例场景下标题都类似，匹配不出「哪一个」）。"""
    u = ctypes.windll.user32
    _prep_window_api(u)
    _activate_hwnd(u, hwnd, wait_ms)


def activate_window(match="Google Chrome", wait_ms=500):
    """把标题含 match 的窗口抬到最前（越过前台锁）。找到返回 True。

    ⚠️ 已知局限（多实例场景，未改）：多个窗口标题都含 match 时，取
    「标题最长」的那一个——这是单实例场景下的启发式，对多实例场景基本
    无效（哪个网页标题字数多纯属巧合，与目标实例无关）。多实例场景应
    优先用 activate_window_by_hwnd，本函数保留仅供单实例场景 /
    未传 --hwnd 时的向后兼容。
    """
    u = ctypes.windll.user32
    _prep_window_api(u)
    wins = [(h, t) for (h, t) in _enum_windows() if match in t]
    if not wins:
        return False
    hwnd = max(wins, key=lambda x: len(x[1]))[0]  # 主窗口取标题最长的
    _activate_hwnd(u, hwnd, wait_ms)
    return True


def locate(gray, tpl_gray, min_scale, max_scale, steps):
    """多尺度匹配，返回 (best_val, left, top, w, h, scale)。"""
    best = None
    H, W = gray.shape[:2]
    for scale in np.linspace(min_scale, max_scale, steps):
        w = int(round(tpl_gray.shape[1] * scale))
        h = int(round(tpl_gray.shape[0] * scale))
        if w < 8 or h < 8 or w > W or h > H:
            continue
        interp = cv2.INTER_AREA if scale < 1 else cv2.INTER_CUBIC
        t = cv2.resize(tpl_gray, (w, h), interpolation=interp)
        res = cv2.matchTemplate(gray, t, cv2.TM_CCOEFF_NORMED)
        _, mx, _, mloc = cv2.minMaxLoc(res)
        if best is None or mx > best[0]:
            best = (float(mx), int(mloc[0]), int(mloc[1]), w, h, float(scale))
    return best


def click(x, y):
    u = ctypes.windll.user32
    u.SetCursorPos(int(x), int(y))
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP = 0x0002, 0x0004
    u.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    u.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)


def main():
    ap = argparse.ArgumentParser(description="定位 Chrome 工具栏 AIChat 扩展图标")
    ap.add_argument("--template", default=DEFAULT_TEMPLATE, help="图标模板 PNG")
    ap.add_argument("--threshold", type=float, default=0.55, help="匹配阈值(0~1)，低于则判为未找到")
    ap.add_argument("--min-scale", type=float, default=0.5)
    ap.add_argument("--max-scale", type=float, default=2.4)
    ap.add_argument("--steps", type=int, default=39)
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    ap.add_argument("--click", action="store_true", help="找到后直接点击")
    ap.add_argument("--activate", action="store_true", help="先把 Chrome 窗口激活到最前再识别")
    ap.add_argument("--activate-match", default="Google Chrome", help="要激活的窗口标题包含串（未传 --hwnd 时生效）")
    ap.add_argument("--activate-wait", type=int, default=500, help="激活后等待毫秒(默认500)")
    ap.add_argument("--save-debug", metavar="PATH", help="保存带命中框的截图以核对")
    ap.add_argument("--region", metavar="LEFT,TOP,W,H",
                     help="把截图与匹配范围收窄到这个矩形（虚拟桌面绝对坐标，与 GetWindowRect 同坐标系）。"
                          "不传则全屏扫描（DEV-125986 之前的行为，向后兼容）")
    ap.add_argument("--hwnd", type=int,
                     help="要激活的窗口句柄（精确指定，优先于 --activate-match）。"
                          "多实例场景下客户端已知道目标 HWND 时应传这个，而不是靠标题匹配")
    args = ap.parse_args()

    if not os.path.isfile(args.template):
        print("ERROR: template not found: %s" % args.template, file=sys.stderr)
        return 1

    region = None
    if args.region:
        try:
            region = parse_region(args.region)
        except ValueError as e:
            print("ERROR: invalid --region: %s" % e, file=sys.stderr)
            return 1

    make_dpi_aware()

    if args.activate:
        if args.hwnd:
            activate_window_by_hwnd(args.hwnd, args.activate_wait)
        elif not activate_window(args.activate_match, args.activate_wait):
            print("ERROR: no window titled *%s* to activate" % args.activate_match,
                  file=sys.stderr)
            return 1

    vx, vy = virtual_origin()

    frame = grab_screen_bgr(region)
    gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
    tpl = imread_unicode(args.template, cv2.IMREAD_GRAYSCALE)
    if tpl is None:
        print("ERROR: cannot read template: %s" % args.template, file=sys.stderr)
        return 1

    best = locate(gray, tpl, args.min_scale, args.max_scale, args.steps)
    if best is None:
        print("ERROR: no scale fit the screen", file=sys.stderr)
        return 2

    val, left, top, w, h, scale = best
    # region 截图的 (0,0) 对应 region 矩形左上角在绝对坐标系里的位置，
    # 需要把 region 的偏移加回去才是真正的绝对坐标；不传 region 时
    # frame 本身就是整个虚拟桌面截图，偏移就是 virtual_origin()
    if region is not None:
        base_x, base_y = region[0], region[1]
    else:
        base_x, base_y = vx, vy
    cx = base_x + left + w // 2
    cy = base_y + top + h // 2

    if args.save_debug:
        dbg = frame.copy()
        cv2.rectangle(dbg, (left, top), (left + w, top + h), (0, 0, 255), 2)
        cv2.drawMarker(dbg, (left + w // 2, top + h // 2), (0, 255, 0),
                       cv2.MARKER_CROSS, 24, 2)
        if not imwrite_unicode(args.save_debug, dbg):
            print("WARN: 无法写出调试图: %s" % args.save_debug, file=sys.stderr)

    if val < args.threshold:
        msg = {"found": False, "confidence": round(val, 3),
               "scale": round(scale, 3), "threshold": args.threshold}
        print(json.dumps(msg) if args.json else
              ("NOT_FOUND confidence=%.3f (<%.2f)" % (val, args.threshold)),
              file=sys.stderr)
        return 2

    if args.click:
        click(cx, cy)

    if args.json:
        print(json.dumps({"found": True, "x": cx, "y": cy,
                          "confidence": round(val, 3), "scale": round(scale, 3),
                          "clicked": bool(args.click)}))
    else:
        print("%d,%d" % (cx, cy))
    return 0


if __name__ == "__main__":
    sys.exit(main())
