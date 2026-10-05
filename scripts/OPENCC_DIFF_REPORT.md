# OpenCC 差分测试报告（Python vs Rust）—— M0 门禁

- 日期：2026-10-05
- 结论先行：**四配置（t2s / s2t / s2tw / s2hk）差异率均为 0.0000%，主运行 28,188 对、补充全量运行 425,576 对比对全部一致。**
- 被测对象：
  - Python 侧：`opencc-python-reimplemented 0.1.7`（原版词典服务的查询词繁简变体扩展库）
  - Rust 侧：`opencc-rs 0.6.2`（`opencc-sys 0.5.2+1.4.1`，vendor OpenCC 1.4.1，字典内嵌）

## 工具与流程

| 文件 | 作用 |
|---|---|
| `crates/server/examples/opencc_dump.rs` | Rust 侧 dump 工具：stdin 按行读词，对 `[T2S, S2T, S2TW, S2HK]` 输出 5 列 TSV（`word\tt2s\ts2t\ts2tw\ts2hk`），转换失败输出空串 |
| `scripts/opencc_diff.py` | 差分脚本：组装测试词表，分别喂 Python 四配置与 Rust 工具，逐词逐配置比对，输出本报告主体 |

复现（在仓库根目录 `/mnt/s790/project/dict` 下，可重复运行，词表采样用固定种子 `20261005`）：

```console
$ python3 scripts/opencc_diff.py                 # 默认运行（含采样）
$ python3 scripts/opencc_diff.py --full-dict     # /usr/share/dict/words 全量
```

一致性验证通过后，输出两次逐字节相同（`diff` 为空）。

## 测试词表构成（主运行共 7047 词）

1. **手造边界词 144 个**：
   - 一对多简繁字：发/髮/發、干/幹/乾、后/後、面/麵/面、里/裡、历/歷/曆、钟/鐘/錶、复/復/複，以及斗/台/只/范/松/谷/几/系/冲/尸/苏/板/表/征/汇/准/划 等各简体字及其对应多个繁体；
   - 词组：头发、皇后、干妈、面子、里面、历史、时钟、复习（及战斗/台风/柜台/一只/示范/轻松/五谷/系统/联系/冲茶/冲突/老板/手表/特征/词汇/标准/计划 等）；
   - 全角/半角混合（ＡＢＣabc、ａｂｃ１２３ 等）；纯英文（hello、Hello World 等）；空串与空白串（""、" "、"  "）；单字重复（一一、发发、干干干、后后后后）；港澳台用词（内存/記憶體、软件/軟體、打印机/印表機、网络/網路、自行车/腳踏車 等）；全角/半角标点（，。！？；：、 与 ,.!?; 及混排句）。
2. **系统词表**：`/usr/share/dict/words`（本机存在，american-english，共 104,334 条）固定种子采样 5,000 条。
3. **随机汉字串 2,000 条**：从约 700 个常用汉字（含全部上述一对多边界字）按固定种子随机拼成长度 1–6 的串，补足中文覆盖率。

## 主运行结果（`python3 scripts/opencc_diff.py`）

- 生成命令：`python3 scripts/opencc_diff.py`（在仓库根目录运行，可重复）
- 总词数：**7047**（手造边界 144 + 系统词表 /usr/share/dict/words 采样 5000/104334 + 随机汉字串 2000），比对总对数（词 × 配置）：**28188**
- Python 侧：opencc-python-reimplemented 0.1.7，配置 t2s/s2t/s2tw/s2hk
- Rust 侧：opencc-rs 0.6.2（vendor OpenCC 1.4.1，字典内嵌），工具 `crates/server/examples/opencc_dump.rs`（debug 构建）

| 配置 | 比对对数 | 一致 | 差异 | 差异率 |
|---|---:|---:|---:|---:|
| t2s | 7047 | 7047 | 0 | 0.0000% |
| s2t | 7047 | 7047 | 0 | 0.0000% |
| s2tw | 7047 | 7047 | 0 | 0.0000% |
| s2hk | 7047 | 7047 | 0 | 0.0000% |
| **合计** | 28188 | 28188 | 0 | 0.0000% |

**四配置全部一致，差异率为 0。** 无差异样本需要归因。

## 补充全量运行（`--full-dict`，系统词表不采样）

| 配置 | 比对对数 | 一致 | 差异 | 差异率 |
|---|---:|---:|---:|---:|
| t2s | 106394 | 106394 | 0 | 0.0000% |
| s2t | 106394 | 106394 | 0 | 0.0000% |
| s2tw | 106394 | 106394 | 0 | 0.0000% |
| s2hk | 106394 | 106394 | 0 | 0.0000% |
| **合计** | 425576 | 425576 | 0 | 0.0000% |

## 结论与归因说明

1. **四配置差异率：t2s 0%、s2t 0%、s2tw 0%、s2hk 0%**（主运行 28,188 对 + 全量 425,576 对）。Python 版与 Rust 版在这四个配置上转换结果完全一致，M0 门禁通过。
2. **为何能一致**：两侧字典同源。opencc-python-reimplemented 内嵌 OpenCC 文本字典（TSCharacters.txt/STCharacters.txt/TSPhrases.txt/STPhrases.txt/TWVariants.txt/HKVariants.txt 等），opencc-rs 经 opencc-sys vendor OpenCC 1.4.1 的同名 .txt 字典；四个非词组配置的字典内容未发生跨版本变动，逐字/逐词最长匹配算法语义一致，故结果一致。
3. **有效性自检**（防止"假通过"，两侧独立运行验证过）：
   - `着`：两侧均为 t2s=着 / s2t=着 / s2tw=**著** / s2hk=着（TW 标准特有异读）；
   - `里面`：两侧均为 s2t/s2hk=**裏**面、s2tw=**裡**面；
   - 空串、空白串、纯英文/ASCII 词两侧均原样返回。
   即差分管线确实在比对两个引擎的真实输出，而非一边复制另一边。
4. **范围边界（差异预期为"可解释"的已知项，本次未纳入四配置）**：
   - 内存→記憶體、打印机→印表機 这类**词组级地区用词转换**只存在于带词组的配置（s2twp/s2hkp 等）。本次门禁的四配置（t2s/s2t/s2tw/s2hk）两侧都只做字级转换：`内存 → 內存`（而非記憶體），Python 与 Rust 行为一致，符合"查询词繁简变体扩展"的原始用法。
   - 若后续门禁扩展到 s2twp/hk2sp 等词组配置，两侧字典版本快照不同（0.1.7 的字典为较早期快照 vs OpenCC 1.4.1），可能出现词组级差异，届时需单独归因。

## 环境备忘

- Python 3.13.15（mise），`pip install --user --break-system-packages opencc-python-reimplemented` 安装 0.1.7。
- Rust 工具链见 `rust-toolchain.toml`；`opencc-rs` 已在 workspace 依赖中（`Cargo.lock` 锁定 0.6.2 / opencc-sys 0.5.2+1.4.1）。
- 本报告由 `scripts/opencc_diff.py` 输出与上述补充说明构成；重跑 `python3 scripts/opencc_diff.py > report.md` 可再生成本文主体数字。
