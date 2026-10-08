# 本地 fork 说明（上游 kokoro-micro 1.3.0，Apache-2.0）

基于 crates.io 的 kokoro-micro 1.3.0，仅为 mydict 服务端的两处需求做加法，
其余与上游逐字节一致：

1. **`TtsEngine::synthesize_pinyin(pinyin, voice, speed, gain)`**
   按拼音合成普通话。词典里 PUA 私有区的字头（说文系古文字字形）在
   「字 → 拼音」字库里没有读音，唯一可靠的发音来源是词典自己印的注音
   （`<py>jīn</py>` / `phonetic` 列）。实现复用上游的
   `g2p::zh::syllable_to_ipa` 音节表，新增 `phonemize_pinyin` 入口
   （带调标记 `yù` 与数字声调 `yu4` 都接受，空白/撇号切音节）。

2. 上游对「用户语速 × 0.65 = 模型语速」的 `SPEED_SCALE` 会让 Kokoro 的
   时长预测器把首音节读成两遍（分布外语速）；mydict 在调用侧传
   `1/0.65` 归一，本 fork 未改动该常量，保持与上游行为一致以便对比。

上游更新时以本文件为清单重新套用补丁。
