"""anycall 아이콘을 만든다: assets/anycall.ico(실행 파일·연결된 파일용)와 assets/icon-64.rgba(창 아이콘).

파란 동영상 화면 위에 흰 재생 표시, 아래에 노란 자막 줄. 외부 패키지 없이 그린다.
    python scripts/make-icon.py
"""
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "assets"
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 4  # 가장자리를 부드럽게 하려고 한 픽셀을 SS×SS로 나눠 칠한다.

TOP = (59, 130, 246)  # 화면 위쪽 색
BOTTOM = (29, 78, 216)  # 화면 아래쪽 색
WHITE = (255, 255, 255)
YELLOW = (250, 204, 21)


def in_round_rect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_triangle(x, y, pts):
    (ax, ay), (bx, by), (cx, cy) = pts
    d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by)
    d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy)
    d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay)
    neg = d1 < 0 or d2 < 0 or d3 < 0
    pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (neg and pos)


def color_at(u, v):
    """u, v는 0~1 좌표. (r, g, b, a) 또는 None(투명)."""
    if not in_round_rect(u, v, 0.03, 0.03, 0.97, 0.97, 0.2):
        return None
    # 자막 줄
    if in_round_rect(u, v, 0.18, 0.70, 0.82, 0.84, 0.07):
        return YELLOW
    # 재생 표시(화면 위쪽 가운데)
    if in_triangle(u, v, [(0.40, 0.18), (0.40, 0.58), (0.68, 0.38)]):
        return WHITE
    t = v
    return tuple(round(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3))


def render(size):
    rgba = bytearray()
    n = SS * SS
    for py in range(size):
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(SS):
                for sx in range(SS):
                    c = color_at((px + (sx + 0.5) / SS) / size, (py + (sy + 0.5) / SS) / size)
                    if c:
                        acc[0] += c[0]
                        acc[1] += c[1]
                        acc[2] += c[2]
                        acc[3] += 1
            if acc[3]:
                rgba += bytes([acc[0] // acc[3], acc[1] // acc[3], acc[2] // acc[3], 255 * acc[3] // n])
            else:
                rgba += b"\0\0\0\0"
    return bytes(rgba)


def png(size, rgba):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    raw = b"".join(b"\0" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    OUT.mkdir(exist_ok=True)
    images = {s: render(s) for s in SIZES}
    pngs = [png(s, images[s]) for s in SIZES]
    header = struct.pack("<HHH", 0, 1, len(SIZES))
    offset = 6 + 16 * len(SIZES)
    entries = b""
    for s, data in zip(SIZES, pngs):
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    (OUT / "anycall.ico").write_bytes(header + entries + b"".join(pngs))
    (OUT / "icon-64.rgba").write_bytes(images[64])
    (OUT / "icon-256.png").write_bytes(pngs[-1])  # 눈으로 확인하는 용도


if __name__ == "__main__":
    main()
