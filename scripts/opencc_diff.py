#!/usr/bin/env python3
"""OpenCC 差分测试（M0 门禁）：Python 版 vs Rust 版转换一致性验证。

Python 侧：opencc-python-reimplemented（原版词典服务所用库），
Rust 侧：opencc-rs 0.6.2（vendor OpenCC 1.4.1，字典内嵌），通过
crates/server/examples/opencc_dump.rs 从 stdin 读词、输出 TSV。

对四个配置 t2s / s2t / s2tw / s2hk 逐词比对，报告输出到 stdout
（markdown 格式，可直接用作 scripts/OPENCC_DIFF_REPORT.md）。

用法（在仓库根目录 /mnt/s790/project/dict 下）：
    python3 scripts/opencc_diff.py [--release] [--no-build] [--full-dict]

可重复运行：词表采样与随机生成均使用固定随机种子。
"""

from __future__ import annotations

import argparse
import importlib.metadata
import random
import string
import subprocess
import sys
from pathlib import Path

from opencc import OpenCC

try:
    PY_OPENCC_VERSION = importlib.metadata.version("opencc-python-reimplemented")
except importlib.metadata.PackageNotFoundError:
    PY_OPENCC_VERSION = "unknown"

REPO_ROOT = Path(__file__).resolve().parent.parent
DICT_WORDS_FILE = Path("/usr/share/dict/words")

CONFIGS = ["t2s", "s2t", "s2tw", "s2hk"]
RANDOM_SEED = 20261005
DICT_SAMPLE_SIZE = 5000      # 从 /usr/share/dict/words 采样的词数
RANDOM_ASCII_WORDS = 5000    # 无系统词表时生成的随机 ASCII 词数
RANDOM_HANZI_WORDS = 2000    # 常用汉字随机串条数

# ---------------------------------------------------------------- 测试词表

# 一对多简繁边界字 + 词组（覆盖 任务书中列举的全部样本）
BOUNDARY_WORDS = [
    # 一对多简繁字
    "发", "髮", "發", "干", "幹", "乾", "后", "後", "面", "麵",
    "里", "裡", "历", "歷", "曆", "钟", "鐘", "錶", "复", "復", "複",
    # 对应词组
    "头发", "皇后", "干妈", "面子", "里面", "历史", "时钟", "复习",
    # 更多一对多边界
    "斗", "鬥", "豆斗", "战斗", "台", "臺", "檯", "台风", "柜台",
    "只", "隻", "衹", "一只", "只有", "范", "範", "范仲淹", "示范",
    "松", "鬆", "松树", "轻松", "谷", "穀", "山谷", "五谷",
    "几", "幾", "茶几", "几个", "系", "係", "繫", "系统", "联系",
    "冲", "衝", "沖", "冲茶", "冲突", "尸", "屍", "尸体",
    "苏", "蘇", "囌", "苏打", "噜苏", "板", "闆", "木板", "老板",
    "表", "錶", "表达", "手表", "征", "徵", "长征", "特征",
    "汇", "匯", "彙", "汇聚", "词汇", "准", "準", "准许", "标准",
    "划", "劃", "畫", "计划", "划船", " beard",  # 留一个前导空格样本
    # 全角/半角混合
    "ＡＢＣabc", "ａｂｃ１２３", "全角ＡＢＣ与半角abc混合",
    "Ｔｅｓｔ词Ｔｅｓｔ", "数字１２３与123", "ＦＵＬＬＷＩＤＴＨ",
    # 纯英文 / 符号
    "hello", "Hello World", "opencc", "ABCdef123",
    # 空串与空白
    "", " ", "  ",
    # 单字重复
    "一一", "发发", "干干干", "后后后后", "面面俱到",
    # 港澳台用词（注意 s2t/s2tw/s2hk 均不含词组转换，预期只做单字转换）
    "内存", "記憶體", "软件", "軟體", "打印机", "印表機",
    "网络", "網路", "自行车", "腳踏車", "数据", "資料",
    # 标点：全角 vs 半角
    "，。！？；：、", ",.!?:;", "你好，世界。", "Hello, world!",
    "中文，English,混合。", "感叹！exclaimed!",
    # 混合长词
    "简繁转换一致性验证", "OpenCC差异测试2026",
    "簡體與繁體互轉", "一對多映射邊界測試",
]

# 常用汉字池（覆盖高频简繁字，含一对多边界字），用于随机串生成
COMMON_HANZI = (
    "的一是了我不人在他有这上们来到时大地为子中你说生国年着就那和要她出"
    "也得里后自以会家可下而过天去能对小多然于心学么之都好看起发当没成只"
    "如事把还用第样道想作种开美总从无情己面最女但现前些所同日手又行意动"
    "方期它头经长儿回位分爱老因很给名法间斯知世什两次使身者被高已亲其进"
    "此话常与活正感见明问力理尔点文几定本公特做外孩相西果走将月十实向声"
    "车全信重三机工物气每并别真打太新比才便夫再书部水像眼等体却加电主界"
    "门利海受听表德少克代员许稜先口由死安写性马光白或住难望教命花结乐色"
    "更拉东神记处让母父应直字场平报友关放至张认接告入笑内英军候民岁往何"
    "度山觉路带万男边风解叫任金快原吃妈变通师立象数四失满战远格士音轻目"
    "条呢病始达深完今提求清王化空业思切怎非找片罗钱语元喜曾离飞科言干流"
    "欢约各即指合反题必该论交终林请医晚制球决传画保读运及则房早院量苦火"
    "布品近坐产答星精视五连司巴奇管类未朋且婚台夜青北队久乎越观落尽形影"
    "红爸百令周吧识步希亚术留市半热送兴造谈容极随演收首根讲整式取照办强"
    "石古华拿计您装似足双妻尼转诉米称丽客南领节衣站黑刻统断福城故历惊脸"
    "选包紧争另建维绝树系伤示愿持千史谁准联妇纪基买志静阿诗独复痛消社算"
    "义竟确酒需单治卡幸兰念举仅钟怕共毛句息功官待究跟穿室易游程号居考突"
    "皮哪费倒价图具刚脑永歌响商礼细专黄块脚味灵改据般破引食仍存众注笔甚"
    "某沉血备习校默务土微娘须试怀料调广苏显赛查密议底列富梦错座参八跑"
    "吧严女摆滑骗既隔层厅款态陈"
)

# 排除词内会破坏 TSV 协议的字符
def _clean(word: str) -> str | None:
    if any(c in word for c in ("\t", "\n", "\r", "\x00")):
        return None
    return word


def load_handmade() -> list[str]:
    words = []
    for w in BOUNDARY_WORDS:
        w = _clean(w)
        if w is not None:
            words.append(w)
    return words


def load_system_dict(rng: random.Random) -> tuple[list[str], str]:
    """优先读 /usr/share/dict/words；否则生成随机 ASCII 词。返回 (词表, 来源说明)。"""
    if DICT_WORDS_FILE.is_file():
        try:
            text = DICT_WORDS_FILE.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            text = ""
        entries = [w for w in (_clean(l.strip()) for l in text.splitlines()) if w]
        if entries:
            total = len(entries)
            if total > DICT_SAMPLE_SIZE:
                entries = rng.sample(entries, DICT_SAMPLE_SIZE)
            return entries, f"系统词表 /usr/share/dict/words 采样 {len(entries)}/{total}"
    # 无系统词表：生成随机 ASCII 词
    words = []
    for _ in range(RANDOM_ASCII_WORDS):
        length = rng.randint(1, 12)
        words.append("".join(rng.choice(string.ascii_letters + string.digits + "'-") for _ in range(length)))
    return words, f"随机生成 ASCII 词 {len(words)} 个"


def load_random_hanzi(rng: random.Random) -> list[str]:
    """常用汉字随机串（长度 1-6），补足中文覆盖率。"""
    pool = COMMON_HANZI.replace(" ", "")
    words = []
    for _ in range(RANDOM_HANZI_WORDS):
        length = rng.randint(1, 6)
        words.append("".join(rng.choice(pool) for _ in range(length)))
    return words


# ---------------------------------------------------------------- Rust 侧

def build_rust_tool(args: list[str]) -> None:
    cmd = ["cargo", "build", "-q", "-p", "server", "--example", "opencc_dump", *args]
    print(f"[diff] building rust tool: {' '.join(cmd)}", file=sys.stderr)
    subprocess.run(cmd, cwd=REPO_ROOT, check=True)


def rust_binary_path(profile: str) -> Path:
    return REPO_ROOT / "target" / profile / "examples" / "opencc_dump"


def run_rust(words: list[str], profile: str) -> list[list[str]]:
    """把全部词喂给 Rust 工具（一次进程调用），返回每词的四配置结果行。"""
    bin_path = rust_binary_path(profile)
    if not bin_path.is_file():
        raise SystemExit(f"rust 工具不存在：{bin_path}（先运行本脚本或 cargo build）")
    payload = "\n".join(words) + "\n"
    proc = subprocess.run(
        [str(bin_path)],
        input=payload,
        capture_output=True,
        text=True,
        cwd=REPO_ROOT,
    )
    if proc.returncode != 0:
        raise SystemExit(f"rust 工具退出码 {proc.returncode}，stderr:\n{proc.stderr}")
    rows: list[list[str]] = []
    out_lines = proc.stdout.split("\n")
    if out_lines and out_lines[-1] == "":
        out_lines.pop()  # 去掉末尾换行产生的空行
    for line in out_lines:
        parts = line.split("\t")
        if len(parts) != 5:
            raise SystemExit(f"rust 工具输出列数异常（期望 5 列 TSV）: {line!r}")
        rows.append(parts)
    if len(rows) != len(words):
        raise SystemExit(f"rust 工具输出行数 {len(rows)} != 输入词数 {len(words)}（注意：词内不可含换行/制表符）")
    return rows


# ---------------------------------------------------------------- Python 侧

def run_python(words: list[str]) -> list[list[str]]:
    converters = {cfg: OpenCC(cfg) for cfg in CONFIGS}
    rows = []
    for w in words:
        rows.append([w] + [converters[c].convert(w) for c in CONFIGS])
    return rows


# ---------------------------------------------------------------- 报告

def fmt_result(s: str) -> str:
    """差异行中的结果可读化：空串与空白显式标注。"""
    if s == "":
        return "〈空串〉"
    if s.strip() == "" and s != "":
        return f"〈空白:{' '.join(f'U+{ord(c):04X}' for c in s)}〉"
    return s


def main() -> None:
    parser = argparse.ArgumentParser(description="OpenCC Python/Rust 差分测试")
    parser.add_argument("--release", action="store_true", help="用 release 构建的 rust 工具")
    parser.add_argument("--no-build", action="store_true", help="跳过 cargo build（直接用已构建的二进制）")
    parser.add_argument("--full-dict", action="store_true", help="不采样，使用 /usr/share/dict/words 全量")
    parser.add_argument("--max-diffs", type=int, default=200, help="报告中逐条列出的最大差异数")
    args = parser.parse_args()

    global DICT_SAMPLE_SIZE
    if args.full_dict:
        DICT_SAMPLE_SIZE = 10**9

    rng = random.Random(RANDOM_SEED)

    handmade = load_handmade()
    dict_words, dict_src = load_system_dict(rng)
    hanzi_words = load_random_hanzi(rng)

    # 去重但保持顺序（空串保留一条）
    seen: set[str] = set()
    words: list[str] = []
    for w in handmade + dict_words + hanzi_words:
        if w not in seen:
            seen.add(w)
            words.append(w)

    profile = "release" if args.release else "debug"
    if not args.no_build or not rust_binary_path(profile).is_file():
        build_rust_tool(["--release"] if args.release else [])

    print(f"[diff] 总词数 {len(words)}（手造边界 {len(handmade)} | {dict_src} | 随机汉字串 {len(hanzi_words)}）", file=sys.stderr)
    print("[diff] python 侧转换中…", file=sys.stderr)
    py_rows = run_python(words)
    print("[diff] rust 侧转换中…", file=sys.stderr)
    rs_rows = run_rust(words, profile)

    # 逐词逐配置比对
    total_pairs = 0
    agree_pairs = 0
    per_config: dict[str, dict[str, int]] = {c: {"total": 0, "agree": 0} for c in CONFIGS}
    diffs: list[tuple[str, str, str, str]] = []  # (word, config, python, rust)
    for py_row, rs_row in zip(py_rows, rs_rows):
        assert py_row[0] == rs_row[0], "词序错位：python/rust 行首词不一致"
        word = py_row[0]
        for i, cfg in enumerate(CONFIGS, start=1):
            total_pairs += 1
            per_config[cfg]["total"] += 1
            if py_row[i] == rs_row[i]:
                per_config[cfg]["agree"] += 1
                agree_pairs += 1
            else:
                diffs.append((word, cfg, py_row[i], rs_row[i]))

    diff_words = {w for w, *_ in diffs}

    # ---------------- 输出 markdown 报告 ----------------
    print()
    print("# OpenCC 差分测试报告（Python vs Rust）")
    print()
    print("- 生成命令：`python3 scripts/opencc_diff.py`（在仓库根目录运行，可重复）")
    print(f"- 总词数：**{len(words)}**（手造边界 {len(handmade)} + {dict_src} + 随机汉字串 {len(hanzi_words)}），"
          f"比对总对数（词 × 配置）：**{total_pairs}**")
    print(f"- Python 侧：opencc-python-reimplemented {PY_OPENCC_VERSION}，配置 t2s/s2t/s2tw/s2hk")
    print(f"- Rust 侧：opencc-rs 0.6.2（vendor OpenCC 1.4.1，字典内嵌），"
          f"工具 `crates/server/examples/opencc_dump.rs`（{profile} 构建）")
    print()
    print("## 结论")
    print()
    print("| 配置 | 比对对数 | 一致 | 差异 | 差异率 |")
    print("|---|---:|---:|---:|---:|")
    for cfg in CONFIGS:
        t = per_config[cfg]["total"]
        a = per_config[cfg]["agree"]
        d = t - a
        rate = d / t * 100 if t else 0.0
        print(f"| {cfg} | {t} | {a} | {d} | {rate:.4f}% |")
    overall = len(diffs) / total_pairs * 100 if total_pairs else 0.0
    print(f"| **合计** | {total_pairs} | {agree_pairs} | {len(diffs)} | {overall:.4f}% |")
    print()
    if not diffs:
        print("**四配置全部一致，差异率为 0。**")
    else:
        print(f"共 {len(diffs)} 对差异，涉及 {len(diff_words)} 个词。全部差异样本（`word | config | python结果 | rust结果`）：")
        print()
        print("```")
        for word, cfg, pv, rv in diffs[: args.max_diffs]:
            print(f"{word} | {cfg} | {fmt_result(pv)} | {fmt_result(rv)}")
        if len(diffs) > args.max_diffs:
            print(f"…（其余 {len(diffs) - args.max_diffs} 条差异省略，"
                  f"可用 --max-diffs 查看全部）")
        print("```")


if __name__ == "__main__":
    main()
