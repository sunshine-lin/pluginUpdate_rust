# ChatGPT 扩展图标定位器（给 ShadowBot 用）

每天用图像识别找出 Chrome 工具栏上 **ChatGPT(OpenAI)扩展图标** 的屏幕坐标，
对**分辨率 / DPI 缩放 / 窗口位置 / 图标排序变化**都免疫（多尺度模板匹配）。

## 文件
- `chatgpt_icon_locator.py` — 主脚本
- `chatgpt_template.png` — 图标模板（28×28，1920×1080@100% 截出）

## 环境
本机 `C:\Program Files\Python310\python.exe` 已自带 `cv2 / numpy / Pillow`，**无需安装**。

## 运行

```bat
python "D:\project\xiaofeng\tools\chatgpt-icon-locator\chatgpt_icon_locator.py"
```

### 输出契约（ShadowBot 靠这个判断）
| 情况 | 标准输出(stdout) | 退出码 |
|---|---|---|
| 找到 | 一行 `x,y`（如 `1678,62`，物理像素绝对坐标） | 0 |
| 没找到（置信度低于阈值） | 空；stderr 打印 NOT_FOUND | 2 |
| 出错（模板缺失等） | 空；stderr 打印 ERROR | 1 |

### 常用参数
- `--activate` **先把 Chrome 窗口自己抬到最前**再识别（内置越过前台锁；省掉 ShadowBot 的激活步）
- `--activate-match "Google Chrome"` 按窗口标题包含串挑要激活的窗口；`--activate-wait 500` 激活后等待毫秒
- `--json` 改输出 JSON：`{"found":true,"x":1678,"y":62,"confidence":0.87,"scale":1.25,...}`
- `--click` 找到后**直接移动鼠标左键点击**（省得把坐标传回 ShadowBot；DPI 自适应）
- `--save-debug d:\tmp\hit.png` 存一张带红框的核对图（排查用）
- `--threshold 0.55` 匹配阈值，默认 0.55（同机同分辨率一般 >0.9；跨机换算后 0.6~0.85）

> `--activate` 找不到 Chrome 窗口时退出码=1；能力集成后，理想的一条命令：
> `python ...\chatgpt_icon_locator.py --activate --click` —— 激活 Chrome、找到图标、直接点，全自动。

## ShadowBot 对接（推荐 A，一步到位）

**A. 激活+识别+点击 全交给脚本（最省事）**
1. ShadowBot 运行命令：`python ...\chatgpt_icon_locator.py --activate --click`
2. 看退出码：0=已点；2=没找到（可截图告警）；1=没找到 Chrome 窗口

**B. 只取坐标回 ShadowBot 再点（要在点击前插动作时用）**
1. ShadowBot 运行命令：`python ...\chatgpt_icon_locator.py --activate`，捕获 stdout
2. 退出码=0 时，把 stdout 按 `,` 切成 x、y → 用 ShadowBot 的「移动鼠标/点击」点 (x,y)

> `--activate` 已内置「把 Chrome 抬到最前 + 越过前台锁 + 等待重绘」。
> 若你的场景里 Chrome 一定已在最前，也可不加 `--activate`（省 0.5s）。

## 维护
- 换了新电脑/新分辨率若偶发 `NOT_FOUND`：在那台机器上重新截一张该图标的干净小图替换 `chatgpt_template.png` 即可（20~30px 见方、含一点白底最好）。
- ChatGPT 扩展换了图标：同样重截模板。
- 想找别的扩展图标：`--template 别的图标.png`，脚本逻辑通用。

## 原理速记
PIL `ImageGrab` 整屏截图 → 转灰度 → OpenCV `TM_CCOEFF_NORMED` 在 0.5×~2.4× 39 档尺度上匹配 →
取最高分；坐标加上虚拟桌面原点偏移得到绝对屏幕坐标。进程设为 per-monitor DPI aware，
所以截图和 `SetCursorPos` 点击都走物理像素，跨缩放不偏。
