# 测试语料夹具

`sine.spx`：440Hz+880Hz 正弦 2 秒，**官方 libspeex 1.2.1**（speexenc）窄带 8kHz 单声道编码——
金标准产物（oxideav-speex 解码器对它的兼容性是集成前提）。

```sh
ffmpeg -f lavfi -i "sine=frequency=440:duration=2" -f lavfi -i "sine=frequency=880:duration=2" \
  -filter_complex "[0][1]amix=inputs=2,volume=0.6" -ar 8000 -ac 1 sine8.wav
speexenc --quality 8 sine8.wav sine.spx
```

A/B 验证记录（解码 vs speexdec 金标准，lag 对齐后相关度）：
NB 0.991 / WB 0.998 / UWB 0.9996，频谱主频与幅度逐点一致。
