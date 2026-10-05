#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""dict 项目测试语料生成器（幂等，重跑覆盖）。

输出到 testdata/（已 gitignore）：

  mdx_v2_basic/   v2 MDX（zlib、UTF-8、无加密）24 词条 + 同名 .mdd（2 个资源）
  mdx_v2_plain/   v2 未压缩 MDX
  mdx_v2_lzo/     v2 LZO 压缩 MDX（需要系统 liblzo2，缺失则跳过）
  mdx_v2_rich/    v2 MDX：Compact=Yes + StyleSheet + 3 条 @@@LINK + 同名词条
  mdx_v1_basic/   v1.2 MDX（32 位几何、未压缩）
  mdx_v1_lzo/     v1.2 LZO 压缩 MDX（需要 liblzo2）
  mdx_v3_basic/   v3 MDX（3.0 + UUID、未加密，块校验和压前计算）
  stardict_basic/ langdao 风格 StarDict（ifo/idx/dict/syn）
  stardict_dz/    StarDict 压缩变体（.dict.dz + .idx.gz）
  ecdict_mini.csv ECDICT 格式 CSV（utf-8-sig）

格式不是照抄文档，而是照两个 reader 的源码写的：
  - v1/v2 MDX/MDD：mdictlib 0.2.8 src/format/{common,v1,v2}
      * 头部：u32 BE XML 长度 + UTF-16LE XML + u32 LE adler32(XML)  ← 注意头部校验和是小端
      * v2 关键词段头 44 字节：5×u64 BE + adler32 BE(前 40 字节)
      * v1 关键词段头 16 字节：4×u32 BE，无校验和；关键词索引裸存（无块信封）
      * 数据块信封：u32 LE 压缩类型(0/1/2) + u32 BE adler32 + 载荷；
        v1/v2 校验和对「解压后」数据算，v3 对「压前载荷」算（见 opendict decompress.rs）
      * v1 几何全 32 位（含词条行里的 u32 记录偏移），v2/v3 全 64 位
  - v3 MDX + StarDict：vendor/opendict/src/{mdict,stardict}
      * v3 = GeneratedByEngineVersion >= 3.0；Encrypted 属性必须是数字（opendict 用 u8 解析），
        0 = 不加密（keys.rs 不强制加密；bit1 直接 Unsupported，bit2 才解密关键词索引）
      * v3 块校验和在解压前的载荷上验证，其余几何与 v2 相同
"""

from __future__ import annotations

import csv
import ctypes
import gzip
import io
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"

# 压缩类型标签（块信封首字段，u32 LE）
COMP_NONE = 0
COMP_LZO = 1
COMP_ZLIB = 2


# ---------------------------------------------------------------------------
# 校验和
# ---------------------------------------------------------------------------

def adler32(data: bytes) -> int:
    return zlib.adler32(data) & 0xFFFFFFFF


def adler32_be(data: bytes) -> bytes:
    """大端 4 字节 adler32（块信封、v2/v3 关键词段头用它存）。"""
    return struct.pack(">I", adler32(data))


# ---------------------------------------------------------------------------
# LZO：ctypes 直调系统 liblzo2（lzo1x_1_compress）
# ---------------------------------------------------------------------------

_LZO = None


def load_lzo():
    """加载 liblzo2，成功返回 True。失败则本脚本跳过 LZO 语料。"""
    global _LZO
    if _LZO is not None:
        return _LZO is not False
    for soname in ("liblzo2.so.2", "liblzo2.so"):
        try:
            lib = ctypes.CDLL(soname)
        except OSError:
            continue
        try:
            lib.lzo1x_1_compress.restype = ctypes.c_int
            lib.lzo1x_1_compress.argtypes = [
                ctypes.c_char_p,                    # src
                ctypes.c_size_t,                    # src_len
                ctypes.c_char_p,                    # dst
                ctypes.POINTER(ctypes.c_size_t),    # dst_len（出入参）
                ctypes.c_void_p,                    # wrkmem
            ]
        except AttributeError:
            continue
        _LZO = lib
        return True
    _LZO = False
    return False


def lzo_compress(data: bytes) -> bytes:
    """lzo1x_1_compress 的裸输出（无长度前缀——mdictlib 的 lzokay 和
    opendict 的 lzo1x crate 都从第 0 字节起解原始 LZO1X 流）。"""
    if _LZO is None:
        raise RuntimeError("liblzo2 未加载")
    dst_cap = len(data) + len(data) // 64 + 16 + 3
    dst = ctypes.create_string_buffer(dst_cap)
    out_len = ctypes.c_size_t(dst_cap)
    # LZO1X_1_MEM_COMPRESS = 16384 * sizeof(lzo_dict_t) = 16384*8（64 位）
    wrkmem = ctypes.create_string_buffer(16384 * 8)
    status = _LZO.lzo1x_1_compress(data, len(data), dst, ctypes.byref(out_len), wrkmem)
    if status != 0:
        raise RuntimeError(f"lzo1x_1_compress 返回 {status}")
    return dst.raw[: out_len.value]


def lzo1x_decompress(src: bytes) -> bytes:
    """LZO1X 解码器——忠实移植 mdictlib 所用 lzokay-2.0.1 的 decompress.rs，
    仅用于生成后自校验（保证 cargo 侧能读回）。"""
    if len(src) < 3:
        raise ValueError("input overrun")

    inp = 0
    out = bytearray()
    state = 0
    lblen = 0

    def take() -> int:
        nonlocal inp
        if inp >= len(src):
            raise ValueError("input overrun")
        b = src[inp]
        inp += 1
        return b

    def take_run(n: int) -> bytes:
        nonlocal inp
        if inp + n > len(src):
            raise ValueError("input overrun")
        chunk = src[inp : inp + n]
        inp += n
        return chunk

    def zero_run() -> int:
        nonlocal inp
        start = inp
        while inp < len(src) and src[inp] == 0:
            inp += 1
        if inp >= len(src):
            raise ValueError("input overrun")
        return inp - start

    inst = take()
    # 首字节是字面量引导：>=22 直接一段字面量；18..21 填 state 计数
    if inst >= 22:
        out += take_run(inst - 17)
        state = 4
    elif inst >= 18:
        out += take_run(inst - 17)
        state = inst - 17

    while True:
        if inp > 1 or state > 0:
            inst = take()
        if inst & 0xC0:
            # [M2] 2KB 内回溯
            nxt = take()
            distance = (nxt << 3) + ((inst >> 2) & 0x7) + 1
            lbcur = len(out) - distance
            if lbcur < 0:
                raise ValueError("lookbehind overrun")
            lblen = (inst >> 5) + 1
            nstate = inst & 0x3
        elif inst & 0x20:
            # [M3] 16KB 内回溯
            lblen = (inst & 0x1F) + 2
            if lblen == 2:
                zeros, tail = zero_run(), take()
                lblen += zeros * 255 + 31 + tail
            raw = struct.unpack_from("<H", take_run(2))[0]
            distance = (raw >> 2) + 1
            lbcur = len(out) - distance
            if lbcur < 0:
                raise ValueError("lookbehind overrun")
            nstate = raw & 0x3
        elif inst & 0x10:
            # [M4] 远回溯 + 终止指令
            lblen = (inst & 0x7) + 2
            if lblen == 2:
                zeros, tail = zero_run(), take()
                lblen += zeros * 255 + 7 + tail
            raw = struct.unpack_from("<H", take_run(2))[0]
            base_dist = ((inst & 0x8) << 11) + (raw >> 2)
            if base_dist == 0:
                break
            distance = base_dist + 16384
            lbcur = len(out) - distance
            if lbcur < 0:
                raise ValueError("lookbehind overrun")
            nstate = raw & 0x3
        else:
            if state == 0:
                # 长字面量
                length = inst + 3
                if length == 3:
                    zeros, tail = zero_run(), take()
                    length += zeros * 255 + 15 + tail
                out += take_run(length)
                state = 4
                continue
            tail = take()
            if state != 4:
                # [M1 short] 2 字节、1KB 内
                distance = (inst >> 2) + (tail << 2) + 1
            else:
                # [M1 long] 3 字节、2..3KB
                distance = (inst >> 2) + (tail << 2) + 2049
            lbcur = len(out) - distance
            if lbcur < 0:
                raise ValueError("lookbehind overrun")
            lblen = 2 if state != 4 else 3
            nstate = inst & 0x3

        if lblen:
            for i in range(lblen):  # 允许重叠拷贝
                out.append(out[lbcur + i])
        if nstate:
            out += take_run(nstate)
        state = nstate

    if lblen != 3:
        raise ValueError("bad terminator")
    if inp != len(src):
        raise ValueError("input not fully consumed" if inp < len(src) else "input overrun")
    return bytes(out)


# ---------------------------------------------------------------------------
# MDX/MDD 通用编码件
# ---------------------------------------------------------------------------

def enc_key(key: str, key_enc: str) -> bytes:
    return key.encode(key_enc)


def terminated(key: str, key_enc: str) -> bytes:
    """NUL 结尾的词头（UTF-8 是 1 字节 0，UTF-16LE 是 2 字节 0）。"""
    return enc_key(key, key_enc) + (b"\x00\x00" if key_enc == "utf-16-le" else b"\x00")


def block_envelope(data: bytes, comp: int, v3: bool) -> bytes:
    """8 字节信封 + 载荷。v1/v2 的 adler32 对解压后数据算；
    v3 对「解压前载荷」算（opendict decompress.rs：v3 校验解密后的压前数据）。"""
    if comp == COMP_ZLIB:
        payload = zlib.compress(data, 9)
    elif comp == COMP_LZO:
        payload = lzo_compress(data)
        if lzo1x_decompress(payload) != data:
            raise AssertionError("LZO 自校验失败（Python 侧解码与原文不一致）")
    else:
        payload = data
    checksum = adler32(payload) if v3 else adler32(data)
    return struct.pack("<I", comp) + struct.pack(">I", checksum) + payload


def header_section(tag: str, attrs: list[tuple[str, str]]) -> bytes:
    """u32 BE 长度 + UTF-16LE XML + u32 LE adler32(XML)。

    头部校验和按小端写——mdictlib header.rs 用 read_u32_le 读它。
    """
    inner = " ".join(f'{name}="{value}"' for name, value in attrs)
    xml = f"<{tag} {inner} />"
    xmlb = xml.encode("utf-16-le")
    if len(xmlb) % 2:
        raise AssertionError("UTF-16LE 长度必为偶数")
    return struct.pack(">I", len(xmlb)) + xmlb + struct.pack("<I", adler32(xmlb))


def chunked(items: list, size: int) -> list[list]:
    return [items[i : i + size] for i in range(0, len(items), size)]


def build_mdict(
    entries: list[tuple[str, bytes]],
    *,
    version: str,                 # "1.2" / "2.0" / "3.0"
    comp: int,
    key_enc: str = "utf-8",
    encoding_label: str = "UTF-8",
    extra_header_attrs: list[tuple[str, str]] | None = None,
    key_block_entries: int = 8,
    record_block_entries: int = 12,
    is_mdd: bool = False,
) -> bytes:
    """组装一个 MDX/MDD 文件（entries 需已按词头排序、值已编码为字节）。

    段布局（各段首尾相接，mdictlib verify_contiguous 会核对）：
      头部段 | 关键词段头 | 关键词索引 | 关键词块 | 记录段头 | 记录索引 | 记录块
    MDX 记录 = 文本 + NUL（两个 reader 都按下一词条偏移切片，mdictlib trim 掉
    尾部 NUL，opendict pop 掉最后一个 NUL）；MDD 资源是裸字节，无分隔符。
    """
    v1 = version.startswith("1.")
    v3 = version.startswith("3.")
    unit = 2 if key_enc == "utf-16-le" else 1

    # ---- 记录流与各词条偏移（相对拼接后的解压流）----
    rec_offsets: list[int] = []
    stream = bytearray()
    for _, value in entries:
        rec_offsets.append(len(stream))
        stream += value
        if not is_mdd:
            stream += b"\x00"

    # ---- 记录块：每块的载荷是块内记录的重新拼接 ----
    record_blocks = chunked(entries, record_block_entries)
    record_index = bytearray()
    record_blobs = bytearray()
    for block in record_blocks:
        blob = b"".join(value if is_mdd else value + b"\x00" for _, value in block)
        envelope = block_envelope(blob, comp, v3)
        record_index += struct.pack(">II" if v1 else ">QQ", len(envelope), len(blob))
        record_blobs += envelope

    # ---- 关键词块与关键词索引 ----
    key_blocks = chunked(entries, key_block_entries)
    key_index = bytearray()
    key_blobs = bytearray()
    base = 0  # 块首词条在整体列表里的序号（用于取记录偏移，同名词条靠位置区分）
    for block in key_blocks:
        rows = bytearray()
        for pos_in_block, (word, _) in enumerate(block):
            prefix = struct.pack(">I" if v1 else ">Q", rec_offsets[base + pos_in_block])
            rows += prefix + terminated(word, key_enc)
        blob = bytes(rows)
        envelope = block_envelope(blob, comp, v3)
        key_blobs += envelope

        first_key, last_key = block[0][0], block[-1][0]
        if v1:
            # v1 关键词索引裸存：u32 词条数 + u8 单位长度 + 词头字节（无结尾符）×2 + u32×2 尺寸
            key_index += struct.pack(">I", len(block))
            for summary in (first_key, last_key):
                raw = enc_key(summary, key_enc)
                units = len(raw) // unit
                if units > 0xFF:
                    raise ValueError("v1 摘要超长")
                key_index += struct.pack(">B", units) + raw
            key_index += struct.pack(">II", len(envelope), len(blob))
        else:
            # v2/v3 规范格式：u64 词条数 + u16 单位长度 + 词头 + 结尾符 ×2 + u64×2 尺寸
            key_index += struct.pack(">Q", len(block))
            for summary in (first_key, last_key):
                raw = enc_key(summary, key_enc)
                key_index += struct.pack(">H", len(raw) // unit) + raw
                key_index += b"\x00" * unit
            key_index += struct.pack(">QQ", len(envelope), len(blob))
        base += len(block)

    # ---- 关键词段头 + 索引 + 块 ----
    if v1:
        # v1：4×u32，无校验和；关键词索引裸存（无块信封）
        keyword_section = (
            struct.pack(">IIII", len(key_blocks), len(entries), len(key_index), len(key_blobs))
            + bytes(key_index)
            + bytes(key_blobs)
        )
    else:
        key_index_envelope = block_envelope(bytes(key_index), comp, v3)
        kw_header = struct.pack(
            ">QQQQQ",
            len(key_blocks),
            len(entries),
            len(key_index),           # 关键词索引解压后长度
            len(key_index_envelope),  # 压缩后长度（含 8 字节信封）
            len(key_blobs),
        )
        keyword_section = (
            kw_header + adler32_be(kw_header) + key_index_envelope + bytes(key_blobs)
        )

    # ---- 记录段头 + 索引 + 块 ----
    pair_size = 8 if v1 else 16
    record_header = struct.pack(
        ">IIII" if v1 else ">QQQQ",
        len(record_blocks),
        len(entries),
        len(record_blocks) * pair_size,
        len(record_blobs),
    )
    record_section = record_header + bytes(record_index) + bytes(record_blobs)

    # ---- 文件头 ----
    attrs = [
        ("GeneratedByEngineVersion", version),
        ("RequiredEngineVersion", version),
        ("Format", "Binary" if is_mdd else "Html"),
        ("KeyCaseSensitive", "No"),
        ("StripKey", "No"),
        ("Encrypted", "0"),
        ("Encoding", encoding_label),
        ("Title", "Generated Fixture"),
        ("Description", "generated by scripts/gen_corpus.py"),
        ("CreationDate", "2026.10.05"),
    ]
    if v3:
        attrs.append(("UUID", "0cf2ae6c-b365-4eaf-bd1d-3e6dc7f09b53"))
    if extra_header_attrs:
        attrs.extend(extra_header_attrs)
    tag = "Library_Data" if is_mdd else "Dictionary"
    return header_section(tag, attrs) + keyword_section + record_section


# ---------------------------------------------------------------------------
# 词条表
# ---------------------------------------------------------------------------

def html_def(word: str, body: str) -> str:
    return f'<div class="hw">{word}</div><div class="dg">{body}</div>'


def basic_entries() -> list[tuple[str, bytes]]:
    """v2 系列共用 24 词条（含中文词头），按码点排序。"""
    pairs = [
        ("apple", html_def("apple", "n. 苹果；一种水果")),
        ("apple tree", html_def("apple tree", "phr. 苹果树")),
        ("application", html_def("application", "n. 应用；申请")),
        ("banana", html_def("banana", "n. 香蕉")),
        ("bank", html_def("bank", "n. 银行；河岸")),
        ("book", html_def("book", "n. 书；书本")),
        ("car", html_def("car", "n. 汽车")),
        ("cat", html_def("cat", "n. 猫")),
        ("China", html_def("China", "n. 中国")),
        ("dictionary", html_def("dictionary", "n. 词典，字典")),
        ("dog", html_def("dog", "n. 狗")),
        ("egg", html_def("egg", "n. 鸡蛋")),
        ("flower", html_def("flower", "n. 花")),
        ("green", html_def("green", "adj. 绿色的")),
        ("hello", html_def("hello", "int. 你好")),
        ("test", html_def("test", "n./v. 测试")),
        ("word", html_def("word", "n. 单词；话语")),
        ("中国", html_def("中国", "the People's Republic of China")),
        ("中国话", html_def("中国话", "Chinese language (spoken)")),
        ("中文", html_def("中文", "n. 中文，汉语")),
        ("你好", html_def("你好", "int. hello; hi")),
        ("汉语", html_def("汉语", "n. Chinese language")),
        ("苹果", html_def("苹果", "n. apple（水果）")),
        ("苹果汁", html_def("苹果汁", "n. apple juice")),
    ]
    # 真实 MDX 按不区分大小写排序（再按原始字节决胜），词序与 mdictlib 的
    # 归一化定位序一致；entry_at(0) 因此是首个小写词条
    return [(w, v.encode("utf-8")) for w, v in sorted(pairs, key=lambda p: (p[0].lower(), p[0]))]


def v1_entries() -> list[tuple[str, bytes]]:
    """v1.2 用 12 词条（同样含 苹果 供 locate 测试）。"""
    pairs = [
        ("apple", html_def("apple", "n. 苹果")),
        ("banana", html_def("banana", "n. 香蕉")),
        ("book", html_def("book", "n. 书")),
        ("car", html_def("car", "n. 汽车")),
        ("cat", html_def("cat", "n. 猫")),
        ("dog", html_def("dog", "n. 狗")),
        ("hello", html_def("hello", "int. 你好")),
        ("test", html_def("test", "n. 测试")),
        ("word", html_def("word", "n. 单词")),
        ("中国", html_def("中国", "China")),
        ("苹果", html_def("苹果", "n. apple")),
        ("苹果汁", html_def("苹果汁", "n. apple juice")),
    ]
    # 真实 MDX 按不区分大小写排序（再按原始字节决胜），词序与 mdictlib 的
    # 归一化定位序一致；entry_at(0) 因此是首个小写词条
    return [(w, v.encode("utf-8")) for w, v in sorted(pairs, key=lambda p: (p[0].lower(), p[0]))]


def v3_entries() -> list[tuple[str, bytes]]:
    pairs = [
        ("apple", html_def("apple", "n. 苹果")),
        ("application", html_def("application", "n. 应用")),
        ("banana", html_def("banana", "n. 香蕉")),
        ("computer", html_def("computer", "n. 计算机，电脑")),
        ("dictionary", html_def("dictionary", "n. 词典")),
        ("engine", html_def("engine", "n. 引擎，发动机")),
        ("future", html_def("future", "n./adj. 未来")),
        ("garden", html_def("garden", "n. 花园")),
        ("hello", html_def("hello", "int. 你好")),
        ("index", html_def("index", "n. 索引")),
        ("中国", html_def("中国", "v3 dictionary entry for China")),
        ("苹果", html_def("苹果", "n. apple")),
    ]
    # 真实 MDX 按不区分大小写排序（再按原始字节决胜），词序与 mdictlib 的
    # 归一化定位序一致；entry_at(0) 因此是首个小写词条
    return [(w, v.encode("utf-8")) for w, v in sorted(pairs, key=lambda p: (p[0].lower(), p[0]))]


def rich_entries() -> list[tuple[str, bytes]]:
    """v2_rich：样式标记词条 + 3 条 @@@LINK（一条两跳链、一条死链）+ 2 个同名词条。"""
    pairs = [
        # 3 条 @@@LINK：appel → apple2 → apple（两跳链）；aple → 不存在的目标
        ("aple", "@@@LINK=nonexistent-entry"),
        ("appel", "@@@LINK=apple2"),
        ("apple", html_def("apple", "`1`n. 苹果`1`；一种水果")),
        ("apple2", "@@@LINK=apple"),
        ("bank", html_def("bank", "n. 银行（金融机构）")),
        ("bank", html_def("bank", "n. 河岸；堤")),
        ("laptop", html_def("laptop", "`1`n. 笔记本电脑`1`")),
        ("orange", html_def("orange", "`2`n. 橙子；橙色`2`")),
        ("pencil", html_def("pencil", "`1`n. 铅笔`1`，`2`词源：拉丁语 penicillus`2`")),
        ("software", html_def("software", "`2`n. 软件`2`")),
        ("中国", html_def("中国", "the People's Republic of China")),
        ("苹果", html_def("苹果", "`1`n. apple`1`（水果）")),
        ("铅笔", html_def("铅笔", "n. pencil")),
    ]
    # 真实 MDX 按不区分大小写排序（再按原始字节决胜），词序与 mdictlib 的
    # 归一化定位序一致；entry_at(0) 因此是首个小写词条
    return [(w, v.encode("utf-8")) for w, v in sorted(pairs, key=lambda p: (p[0].lower(), p[0]))]


# ---------------------------------------------------------------------------
# 各语料
# ---------------------------------------------------------------------------

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def make_png() -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)  # 1x1 RGBA8
    idat = zlib.compress(b"\x00\xff\x00\x00\xff", 9)     # filter 0 + 1 像素
    return PNG_MAGIC + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def gen_mdx_v2_basic() -> None:
    out = TESTDATA / "mdx_v2_basic"
    out.mkdir(parents=True, exist_ok=True)
    (out / "basic.mdx").write_bytes(
        build_mdict(basic_entries(), version="2.0", comp=COMP_ZLIB)
    )
    # 同名 MDD：2 个资源，词头 UTF-16LE（mdictlib 对 MDD 恒用 UTF-16LE）
    css = b"body { font-family: serif; }\n.hw { font-weight: bold; }\n"
    png = make_png()
    resources = sorted(
        [(r"\style.css", css), (r"\img\logo.png", png)], key=lambda p: p[0]
    )
    (out / "basic.mdd").write_bytes(
        build_mdict(
            resources,
            version="2.0",
            comp=COMP_ZLIB,
            key_enc="utf-16-le",
            encoding_label="UTF-16LE",
            is_mdd=True,
            key_block_entries=8,
            record_block_entries=8,
        )
    )


def gen_mdx_v2_plain() -> None:
    out = TESTDATA / "mdx_v2_plain"
    out.mkdir(parents=True, exist_ok=True)
    (out / "plain.mdx").write_bytes(
        build_mdict(basic_entries(), version="2.0", comp=COMP_NONE)
    )


def gen_mdx_v2_lzo() -> None:
    out = TESTDATA / "mdx_v2_lzo"
    out.mkdir(parents=True, exist_ok=True)
    (out / "lzo.mdx").write_bytes(
        build_mdict(basic_entries(), version="2.0", comp=COMP_LZO)
    )


def gen_mdx_v2_rich() -> None:
    out = TESTDATA / "mdx_v2_rich"
    out.mkdir(parents=True, exist_ok=True)
    # StyleSheet：编号行/开始标签行/结束标签行交替（属性值里不用双引号，免得
    # opendict 的朴素 XML 扫描器提前截断）
    stylesheet = "1\n<b>\n</b>\n2\n<font color=#0000cc>\n</font>\n"
    (out / "rich.mdx").write_bytes(
        build_mdict(
            rich_entries(),
            version="2.0",
            comp=COMP_ZLIB,
            extra_header_attrs=[("Compact", "Yes"), ("StyleSheet", stylesheet)],
            key_block_entries=5,
            record_block_entries=5,
        )
    )


def gen_mdx_v1_basic() -> None:
    out = TESTDATA / "mdx_v1_basic"
    out.mkdir(parents=True, exist_ok=True)
    (out / "v1.mdx").write_bytes(
        build_mdict(v1_entries(), version="1.2", comp=COMP_NONE)
    )


def gen_mdx_v1_lzo() -> None:
    out = TESTDATA / "mdx_v1_lzo"
    out.mkdir(parents=True, exist_ok=True)
    (out / "v1lzo.mdx").write_bytes(
        build_mdict(v1_entries(), version="1.2", comp=COMP_LZO)
    )


def gen_mdx_v3_basic() -> None:
    out = TESTDATA / "mdx_v3_basic"
    out.mkdir(parents=True, exist_ok=True)
    # v3 未加密：Encrypted="0"（opendict 用 u8 解析该属性），keys.rs 对 bit1 直接
    # Unsupported、bit2 才用全局密钥解密关键词索引——即 v3 不强制加密。
    # 带上 UUID（密钥派生输入），但加密位为 0，全局密钥不会被用到。
    (out / "v3.mdx").write_bytes(
        build_mdict(v3_entries(), version="3.0", comp=COMP_ZLIB)
    )


# ---------------------------------------------------------------------------
# StarDict
# ---------------------------------------------------------------------------

def stardict_sort_key(word: str):
    """opendict strcmp.rs 的 stardict_strcmp = g_ascii_strcasecmp 再原始字节决胜；
    bytes.lower() 恰好只小写 ASCII，与之一致。"""
    raw = word.encode("utf-8")
    return (raw.lower(), raw)


def build_stardict_files(entries: list[tuple[str, str]], synonyms: list[tuple[str, int]]):
    """返回 (idx_bytes, dict_bytes, syn_bytes)。sametypesequence=m：正文整块即数据。"""
    idx = bytearray()
    blob = bytearray()
    for word, meaning in sorted(entries, key=lambda e: stardict_sort_key(e[0])):
        data = meaning.encode("utf-8")
        idx += word.encode("utf-8") + b"\x00"
        idx += struct.pack(">II", len(blob), len(data))
        blob += data
    syn = bytearray()
    for alias, target in sorted(synonyms, key=lambda s: stardict_sort_key(s[0])):
        syn += alias.encode("utf-8") + b"\x00" + struct.pack(">I", target)
    return bytes(idx), bytes(blob), bytes(syn)


def gen_stardict_basic() -> None:
    out = TESTDATA / "stardict_basic"
    out.mkdir(parents=True, exist_ok=True)
    entries = [
        ("ability", "n. 能力；才能"),
        ("apple", "n. 苹果"),
        ("banana", "n. 香蕉"),
        ("carbon", "n. 碳"),
        ("card", "n. 卡片；名片"),
        ("cardigan", "n. 开襟羊毛衫"),
        ("dictionary", "n. 词典，字典"),
        ("yellow", "a. 黄色的 n. 黄色"),
    ]
    sorted_words = [w for w, _ in sorted(entries, key=lambda e: stardict_sort_key(e[0]))]
    synonyms = [
        ("apple fruit", sorted_words.index("apple")),
        ("lexicon", sorted_words.index("dictionary")),
    ]
    idx, blob, syn = build_stardict_files(entries, synonyms)
    ifo = (
        "StarDict's dict ifo file\n"
        "version=2.4.2\n"
        "bookname=Langdao EC Mini (朗道英汉迷你)\n"
        "wordcount=%d\n"
        "idxfilesize=%d\n"
        "sametypesequence=m\n"
        "synwordcount=%d\n"
        "description=generated by scripts/gen_corpus.py\n"
        "date=2026.10.05\n" % (len(entries), len(idx), len(synonyms))
    )
    (out / "test.ifo").write_text(ifo, encoding="utf-8")
    (out / "test.idx").write_bytes(idx)
    (out / "test.dict").write_bytes(blob)
    (out / "test.syn").write_bytes(syn)


def gen_stardict_dz() -> None:
    out = TESTDATA / "stardict_dz"
    out.mkdir(parents=True, exist_ok=True)
    entries = [
        ("advance", "v./n. 前进；预付"),
        ("apple", "n. 苹果"),
        ("banana", "n. 香蕉"),
        ("garden", "n. 花园"),
        ("zebra", "n. 斑马"),
    ]
    idx, blob, _ = build_stardict_files(entries, [])
    ifo = (
        "StarDict's dict ifo file\n"
        "version=3.0.0\n"
        "bookname=Langdao EC Mini DZ\n"
        "wordcount=%d\n"
        "idxfilesize=%d\n"
        "sametypesequence=m\n"
        "synwordcount=0\n" % (len(entries), len(idx))
    )
    (out / "test.ifo").write_text(ifo, encoding="utf-8")
    # dictzip 与 gzip 兼容；reader 端 io.rs/dict.rs 都用 GzDecoder，纯 gzip 即可。
    # mtime=0 保证重跑字节稳定（幂等）。
    (out / "test.dict.dz").write_bytes(gzip.compress(blob, 9, mtime=0))
    (out / "test.idx.gz").write_bytes(gzip.compress(idx, 9, mtime=0))


# ---------------------------------------------------------------------------
# ECDICT CSV
# ---------------------------------------------------------------------------

ECDICT_HEADER = [
    "word", "phonetic", "definition", "translation", "pos", "collins", "oxford",
    "tag", "bnc", "frq", "exchange", "detail", "audio",
]


def gen_ecdict_csv() -> None:
    # 注意：translation 里的 \\n 是「字面反斜杠 + n」两个字符——ECDICT 用它做换行占位
    rows = [
        ["apple", "ˈæpl", "an edible round fruit", "n. 苹果\\n一种水果", "n", "3", "1",
         "zk gk", "3499", "1500", "s/apples", "", ""],
        ["banana", "bəˈnɑːnə", "", "n. 香蕉", "n", "2", "1", "zk gk", "3527", "2000", "", "", ""],
        ["water", "ˈwɔːtə", "", "n. 水\\nv. 浇水", "n", "4", "1", "zk cet4", "1200", "3000",
         "d/watered/s/waters", "", ""],
        ["run", "rʌn", "move fast on foot", "v. 跑；运转\\nn. 跑步", "v", "5", "1",
         "cet4 ky", "1111", "2500", "d/ran/p/run/i/running/s/runs", "", ""],
        ["good", "ɡʊd", "", "a. 好的\\nint. 好！", "a", "5", "1", "zk cet4 ky", "666", "700",
         "c/better/b/best", "", ""],
        ["test", "test", "a procedure intended to establish quality", "n./v. 测试\\n试验",
         "n", "1", "0", "zk", "88", "900", "", "", ""],
        ["data", "ˈdeɪtə", "", "n. 数据；资料", "n", "3", "1", "cet4 ky", "1500", "1600", "",
         '{"w": "data", "countable": false}', ""],
        ["happy", "ˈhæpi", "", "a. 高兴的；幸福的", "a", "3", "1", "zk gk", "900", "1100", "", "", ""],
        ["python", "ˈpaɪθɑːn", "", "n. 蟒蛇\\nPython 语言", "n", "1", "0", "ky", "700", "500", "", "", ""],
        ["zebra", "ˈziːbrə", "", "n. 斑马", "n", "1", "0", "gk", "1000", "400", "", "", ""],
    ]
    path = TESTDATA / "ecdict_mini.csv"
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\n")
    writer.writerow(ECDICT_HEADER)
    writer.writerows(rows)
    with open(path, "w", encoding="utf-8-sig", newline="") as fh:
        fh.write(buf.getvalue())

    # 自校验：BOM、列数、字面 \n 与 JSON detail
    text = path.read_text(encoding="utf-8-sig")
    assert path.read_bytes()[:3] == b"\xef\xbb\xbf", "ECDICT CSV 缺 utf-8-sig BOM"
    parsed = list(csv.reader(io.StringIO(text)))
    assert parsed[0] == ECDICT_HEADER
    assert len(parsed) == 11, f"ECDICT 应有表头 + 10 行，实际 {len(parsed)}"
    for i, row in enumerate(parsed[1:], 1):
        assert len(row) == 13, f"第 {i} 行列数 {len(row)} != 13"
    by_word = {row[0]: row for row in parsed[1:]}
    assert "\\n" in by_word["apple"][3], "apple 的 translation 应含字面 \\n"
    detail = by_word["data"][11]
    import json
    obj = json.loads(detail)
    assert obj["w"] == "data" and obj["countable"] is False


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------

def main() -> int:
    TESTDATA.mkdir(parents=True, exist_ok=True)
    lzo_ok = load_lzo()
    if lzo_ok:
        print("liblzo2: 已加载，生成 LZO 语料")
    else:
        print("liblzo2: 不可用（apt-get install -y liblzo2 可补），跳过 LZO 语料")

    gen_mdx_v2_basic()
    gen_mdx_v2_plain()
    if lzo_ok:
        gen_mdx_v2_lzo()
        gen_mdx_v1_lzo()
    gen_mdx_v2_rich()
    gen_mdx_v1_basic()
    gen_mdx_v3_basic()
    gen_stardict_basic()
    gen_stardict_dz()
    gen_ecdict_csv()

    print(f"语料已生成到 {TESTDATA}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
