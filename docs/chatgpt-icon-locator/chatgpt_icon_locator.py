# -*- coding: utf-8 -*-
"""
每天识别 Chrome 工具栏上「ChatGPT(OpenAI)扩展图标」的屏幕坐标。
方法：整屏截图 + OpenCV 多尺度模板匹配 —— 对分辨率 / DPI 缩放 / 窗口位置免疫。
给 ShadowBot 用：默认在标准输出打印一行 `x,y`（物理像素、绝对屏幕坐标），
找不到则退出码=2，出错退出码=1。

依赖：opencv-python(cv2)、numpy、Pillow —— 本机 Python 3.10 均已自带，无需安装。

用法示例：
  python chatgpt_icon_locator.py                 # 打印 "1678,62"
  python chatgpt_icon_locator.py --json          # 打印 JSON(含置信度/尺度)
  python chatgpt_icon_locator.py --click         # 直接移动鼠标并左键点击
  python chatgpt_icon_locator.py --save-debug d:\tmp\hit.png   # 存一张带命中框的图核对
  python chatgpt_icon_locator.py --threshold 0.55 --template 别的图标.png
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
DEFAULT_TEMPLATE = os.path.join(HERE, "chatgpt_template.png")


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


def grab_screen_bgr():
    img = ImageGrab.grab(all_screens=True)  # 整个虚拟桌面
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


def activate_window(match="Google Chrome", wait_ms=500):
    """把标题含 match 的窗口抬到最前（越过前台锁）。找到返回 True。"""
    u = ctypes.windll.user32
    for f in ("IsIconic", "ShowWindow", "SwitchToThisWindow",
              "BringWindowToTop", "SetForegroundWindow"):
        getattr(u, f).argtypes = [ctypes.c_void_p] + \
            ([ctypes.c_int] if f == "ShowWindow" else
             [ctypes.c_bool] if f == "SwitchToThisWindow" else [])
    wins = [(h, t) for (h, t) in _enum_windows() if match in t]
    if not wins:
        return False
    hwnd = max(wins, key=lambda x: len(x[1]))[0]  # 主窗口取标题最长的
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
    ap = argparse.ArgumentParser(description="定位 Chrome 工具栏 ChatGPT 扩展图标")
    ap.add_argument("--template", default=DEFAULT_TEMPLATE, help="图标模板 PNG")
    ap.add_argument("--threshold", type=float, default=0.55, help="匹配阈值(0~1)，低于则判为未找到")
    ap.add_argument("--min-scale", type=float, default=0.5)
    ap.add_argument("--max-scale", type=float, default=2.4)
    ap.add_argument("--steps", type=int, default=39)
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    ap.add_argument("--click", action="store_true", help="找到后直接点击")
    ap.add_argument("--activate", action="store_true", help="先把 Chrome 窗口激活到最前再识别")
    ap.add_argument("--activate-match", default="Google Chrome", help="要激活的窗口标题包含串")
    ap.add_argument("--activate-wait", type=int, default=500, help="激活后等待毫秒(默认500)")
    ap.add_argument("--save-debug", metavar="PATH", help="保存带命中框的截图以核对")
    args = ap.parse_args()

    if not os.path.isfile(args.template):
        print("ERROR: template not found: %s" % args.template, file=sys.stderr)
        return 1

    make_dpi_aware()

    if args.activate:
        if not activate_window(args.activate_match, args.activate_wait):
            print("ERROR: no window titled *%s* to activate" % args.activate_match,
                  file=sys.stderr)
            return 1

    vx, vy = virtual_origin()

    frame = grab_screen_bgr()
    gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
    tpl = cv2.imread(args.template, cv2.IMREAD_GRAYSCALE)
    if tpl is None:
        print("ERROR: cannot read template: %s" % args.template, file=sys.stderr)
        return 1

    best = locate(gray, tpl, args.min_scale, args.max_scale, args.steps)
    if best is None:
        print("ERROR: no scale fit the screen", file=sys.stderr)
        return 2

    val, left, top, w, h, scale = best
    cx = vx + left + w // 2
    cy = vy + top + h // 2

    if args.save_debug:
        dbg = frame.copy()
        cv2.rectangle(dbg, (left, top), (left + w, top + h), (0, 0, 255), 2)
        cv2.drawMarker(dbg, (left + w // 2, top + h // 2), (0, 255, 0),
                       cv2.MARKER_CROSS, 24, 2)
        cv2.imwrite(args.save_debug, dbg)

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
