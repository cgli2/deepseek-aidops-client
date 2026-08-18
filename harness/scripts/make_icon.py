#!/usr/bin/env python3
"""生成 aidops-desktop 的品牌化可执行文件图标 (icon.ico)。

设计：圆角渐变底（深靛蓝 #2A3F8F -> 青色 #14B8C4）+ 白色 "ops" 字母组合。
- 每个目标尺寸独立绘制（而非放大缩放），保证 16px 等小尺寸依旧清晰。
- 输出为包含 16/24/32/48/64/128/256 多档分辨率的 Windows .ico。

用法：
    python scripts/make_icon.py
产物：
    harness/bin/assets/icon.ico
要换字母 / 配色 / 换成自己的 logo，改下面的 BRAND_TEXT / COLORS 或直接替换 icon.ico 即可。
"""
import io
import os
import struct
from PIL import Image, ImageDraw, ImageFont

ASSETS = os.path.join(os.path.dirname(__file__), "..", "bin", "assets")
OUT = os.path.abspath(os.path.join(ASSETS, "icon.ico"))

BRAND_TEXT = "ops"         # 字母组合（ops = AIOps，规避外部品牌商标风险）
TOP = (42, 63, 143)        # 渐变顶部：深靛蓝 #2A3F8F
BOT = (20, 184, 196)       # 渐变底部：青色 #14B8C4
GLYPH = (255, 255, 255)    # 字母颜色：白

SIZES = [16, 24, 32, 48, 64, 128, 256]


def load_font(sz: int) -> ImageFont.ImageFont:
    for p in (
        "C:/Windows/Fonts/arialbd.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ):
        if os.path.exists(p):
            try:
                return ImageFont.truetype(p, sz)
            except Exception:
                pass
    return ImageFont.load_default()


def render(size: int) -> Image.Image:
    s = size
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # 1) 垂直渐变底
    grad = Image.new("RGBA", (s, s))
    px = grad.load()
    for y in range(s):
        t = y / max(1, s - 1)
        r = int(TOP[0] + (BOT[0] - TOP[0]) * t)
        g = int(TOP[1] + (BOT[1] - TOP[1]) * t)
        b = int(TOP[2] + (BOT[2] - TOP[2]) * t)
        for x in range(s):
            px[x, y] = (r, g, b, 255)

    # 2) 圆角遮罩（透明四角）
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, s - 1, s - 1], radius=max(2, int(s * 0.22)), fill=255
    )
    grad.putalpha(mask)

    d = ImageDraw.Draw(grad)

    # 3) 顶部高光（轻微光泽）
    hl = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(hl).rounded_rectangle(
        [int(s * 0.08), int(s * 0.06), int(s * 0.92), int(s * 0.52)],
        radius=max(2, int(s * 0.18)),
        fill=(255, 255, 255, int(38 * (s / 256) + 14)),
    )
    grad = Image.alpha_composite(hl, grad)

    # 4) 字母组合（字号按字符数自适应，避免长文本在小尺寸溢出圆角）
    d = ImageDraw.Draw(grad)
    font = load_font(max(int(s * 0.52 * (2.0 / max(1, len(BRAND_TEXT)))), 7))
    bbox = d.textbbox((0, 0), BRAND_TEXT, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    tx = (s - tw) / 2 - bbox[0]
    ty = (s - th) / 2 - bbox[1] - s * 0.02  # 视觉居中微调
    d.text((tx, ty), BRAND_TEXT, font=font, fill=(*GLYPH, 255))

    return grad


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
    frames = [render(s) for s in SIZES]
    save_ico(OUT, frames)
    save_rust(ICON_DATA_RS, ICON_DATA_SIZE)
    print("wrote", os.path.normpath(OUT), "sizes:", SIZES)
    print("wrote", os.path.normpath(ICON_DATA_RS))


if __name__ == "__main__":
    main()
