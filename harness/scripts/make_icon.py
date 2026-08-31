#!/usr/bin/env python3
"""生成 aidops-desktop 的 Windows、macOS 和运行时图标。

设计：深色圆角底 + 原创「智能脉冲结」标记。双轨数据流围绕中心节点形成
抽象 A，表达 AI 推理、可观测信号和运维闭环；不依赖字体，小尺寸仍清晰。
- 每个目标尺寸以 4x 超采样独立绘制，保证 16px 图标边缘干净。
- 输出 Windows 多尺寸 .ico、macOS 1024px PNG 源图和 egui RGBA 数据。

用法：
    python scripts/make_icon.py
产物：
    harness/bin/assets/icon.ico
    harness/bin/assets/icon_1024.png
横向品牌字标源文件位于 bin/assets/aidops-logo.svg。
"""
import io
import os
import struct
import sys
from PIL import Image, ImageDraw

ASSETS = os.path.join(os.path.dirname(__file__), "..", "bin", "assets")
OUT = os.path.abspath(os.path.join(ASSETS, "icon.ico"))
MAC_SOURCE = os.path.abspath(os.path.join(ASSETS, "icon_1024.png"))
MAC_ICNS = os.path.abspath(os.path.join(ASSETS, "AppIcon.icns"))

INK = (8, 17, 31)
INK_LIGHT = (15, 31, 52)
BLUE = (96, 165, 250)
MINT = (94, 234, 212)
WHITE = (240, 249, 255)

SIZES = [16, 24, 32, 48, 64, 128, 256]


def render(size: int) -> Image.Image:
    scale = 4
    s = size * scale
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # 克制的深色场，保留 macOS 图标所需的大圆角和安全边距。
    grad = Image.new("RGBA", (s, s))
    px = grad.load()
    for y in range(s):
        t = y / max(1, s - 1)
        r = int(INK_LIGHT[0] + (INK[0] - INK_LIGHT[0]) * t)
        g = int(INK_LIGHT[1] + (INK[1] - INK_LIGHT[1]) * t)
        b = int(INK_LIGHT[2] + (INK[2] - INK_LIGHT[2]) * t)
        for x in range(s):
            px[x, y] = (r, g, b, 255)

    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [int(s * 0.035), int(s * 0.035), int(s * 0.965), int(s * 0.965)],
        radius=max(2, int(s * 0.225)),
        fill=255,
    )
    grad.putalpha(mask)
    d = ImageDraw.Draw(grad)

    # 双轨脉冲结：两条折线共享中心节点，构成抽象 A/闭环。
    width = max(scale * 2, int(s * 0.055))
    left = [(s * .25, s * .64), (s * .39, s * .37), (s * .53, s * .58), (s * .73, s * .30)]
    right = [(s * .27, s * .75), (s * .47, s * .47), (s * .61, s * .69), (s * .76, s * .49)]
    d.line(left, fill=BLUE, width=width, joint="curve")
    d.line(right, fill=MINT, width=width, joint="curve")

    # 三个端点代表输入、推理和动作，中心白点强化小尺寸辨识。
    radius = max(scale * 2, int(s * .045))
    for x, y, color in [
        (*left[0], BLUE),
        (*left[-1], BLUE),
        (*right[0], MINT),
        (*right[-1], MINT),
    ]:
        d.ellipse([x-radius, y-radius, x+radius, y+radius], fill=color)
    cx, cy = right[1]
    d.ellipse([cx-radius, cy-radius, cx+radius, cy+radius], fill=WHITE)

    return grad.resize((size, size), Image.Resampling.LANCZOS)


def save_ico(path: str, frames) -> None:
    """手动拼装多尺寸 .ico（PNG 压缩，Vista+ 标准）。

    Pillow 12.x 的 ICO 多帧写入有 bug（append_images 被忽略，只落首帧），
    故直接按 ICONDIR + ICONDIRENTRY 规范拼装各尺寸 PNG。rc.exe / Explorer 均支持。
    """
    pngs = []
    for im in frames:
        buf = io.BytesIO()
        im.save(buf, format="PNG")
        pngs.append(buf.getvalue())

    count = len(pngs)
    out = io.BytesIO()
    out.write(struct.pack("<HHH", 0, 1, count))  # ICONDIR: reserved, type=1, count
    pos = 6 + 16 * count  # 数据区起始偏移
    for im, png in zip(frames, pngs):
        w, h = im.size
        bw = 0 if w >= 256 else w  # 0 表示 256（字节上限）
        bh = 0 if h >= 256 else h
        out.write(struct.pack("<BBBBHHII", bw, bh, 0, 0, 1, 32, len(png), pos))
        pos += len(png)
    for png in pngs:
        out.write(png)

    with open(path, "wb") as f:
        f.write(out.getvalue())


# 窗口 / 任务栏图标尺寸（egui IconData 直出，避免运行时引入图片解码依赖）。
ICON_DATA_SIZE = 64
ICON_DATA_RS = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "harness-ui", "src", "icon_data.rs")
)


def save_rust(path: str, size: int) -> None:
    """把单帧图标的 RGBA 字节烤成 Rust 源文件，供运行时喂给 egui::IconData。"""
    img = render(size).convert("RGBA")
    rgba = list(img.tobytes())  # 行主序、左上起、RGBA 直出（egui 所需格式）
    body = ", ".join(str(b) for b in rgba)
    src = (
        "// 自动生成，请勿手改。由 scripts/make_icon.py 生成（"
        f"{size}x{size} 窗口/任务栏图标 RGBA）。\n"
        "// 运行时直接喂给 egui::IconData，避免引入图片解码依赖。\n"
        "// 重新生成：修改 scripts/make_icon.py 后运行 `python scripts/make_icon.py`。\n\n"
        "#[allow(dead_code)]\n"
        f"pub const APP_ICON_WIDTH: u32 = {size};\n"
        "#[allow(dead_code)]\n"
        f"pub const APP_ICON_HEIGHT: u32 = {size};\n"
        "#[allow(dead_code)]\n"
        "pub const APP_ICON_RGBA: &[u8] = &[\n    " + body + "\n];\n"
    )
    with open(path, "w", encoding="utf-8") as f:
        f.write(src)


def main() -> None:
    os.makedirs(ASSETS, exist_ok=True)
    if "--mac-only" in sys.argv:
        mac_icon = render(1024)
        mac_icon.save(MAC_SOURCE, format="PNG")
        mac_icon.save(MAC_ICNS, format="ICNS")
        print("wrote", os.path.normpath(MAC_SOURCE))
        print("wrote", os.path.normpath(MAC_ICNS))
        return
    frames = [render(s) for s in SIZES]
    save_ico(OUT, frames)
    mac_icon = render(1024)
    mac_icon.save(MAC_SOURCE, format="PNG")
    mac_icon.save(MAC_ICNS, format="ICNS")
    save_rust(ICON_DATA_RS, ICON_DATA_SIZE)
    print("wrote", os.path.normpath(OUT), "sizes:", SIZES)
    print("wrote", os.path.normpath(MAC_SOURCE))
    print("wrote", os.path.normpath(MAC_ICNS))
    print("wrote", os.path.normpath(ICON_DATA_RS))


if __name__ == "__main__":
    main()
