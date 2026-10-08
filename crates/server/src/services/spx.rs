//! Speex(.spx) → WAV 按需解码（纯 Rust：`ogg` 解容器 + `oxideav-speex` 解码）。
//!
//! MDict 词典的人声发音常见 `.spx`（Ogg 封装的 Speex），浏览器放不了——
//! 请求落进 `/dict-res` 时服务端现解成 WAV 返回，产物落盘（`x.spx.wav`）
//! 下次直接命中。取代旧的外部工具链（speexdec → wav → lame → mp3）：
//! 零外部运行时依赖，Windows 也能用，且省掉一层有损转码。

use std::io::Cursor;

/// 把 `.spx` 字节解成 WAV 字节（16-bit PCM，声道/采样率跟原文件）。
/// 输入不是合法的 Ogg/Speex 时返回 Err（调用方按资源缺失处理）。
pub fn decode_spx_to_wav(spx: &[u8]) -> Result<Vec<u8>, String> {
    // 1) Ogg 解容器取包（MDict 的 .spx 是单逻辑流）
    let mut reader = ogg::PacketReader::new(Cursor::new(spx));
    let mut packets: Vec<Vec<u8>> = Vec::new();
    while let Some(packet) = reader
        .read_packet()
        .map_err(|e| format!("Ogg 解包失败：{e}"))?
    {
        packets.push(packet.data);
    }
    if packets.is_empty() {
        return Err("Ogg 里一个包都没有".to_string());
    }

    // 2) 首包 = Speex 头（SPEEX_MAGIC + 13 个 LE i32）
    let header =
        oxideav_speex::SpeexHeader::parse(&packets[0]).map_err(|e| format!("Speex 头无效：{e}"))?;
    let mut decoder = oxideav_speex::SpeexStreamDecoder::for_header(&header)
        .map_err(|e| format!("Speex 流初始化失败：{e}"))?;
    let rate = decoder.output_rate_hz();
    let channels = header.nb_channels.max(1) as u16;

    // 3) 第二个包是注释包（跳过），其余都是音频帧
    let mut pcm: Vec<i16> = Vec::new();
    for packet in packets.iter().skip(1) {
        match decoder.decode_packet_pcm_i16(packet) {
            Ok(samples) => pcm.extend(samples),
            // 个别坏包跳过（丢一帧比整条音频 404 好）
            Err(_) => continue,
        }
    }
    if pcm.is_empty() {
        return Err("没有解出任何音频帧".to_string());
    }

    Ok(pcm_to_wav(&pcm, rate, channels))
}

/// f32→WAV 的姊妹实现：i16 PCM 直接落字节
fn pcm_to_wav(pcm: &[i16], rate: u32, channels: u16) -> Vec<u8> {
    let data_len = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes()); // 字节率
    out.extend_from_slice(&(channels * 2).to_le_bytes()); // 块对齐
    out.extend_from_slice(&16u16.to_le_bytes()); // 位深
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具由 gen_sine_fixture（--ignored 跑一次）生成，产物入库
    #[test]
    fn decodes_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sine.spx");
        let spx = std::fs::read(path).expect("夹具缺失（先跑 gen_sine_fixture --ignored）");
        let wav = decode_spx_to_wav(&spx).expect("解码失败");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(rate, 8000, "窄带 8kHz");
        // 2 秒 ± 容差（帧对齐）
        let samples = (wav.len() - 44) / 2;
        assert!(samples > 15000 && samples < 17000, "样本数 {samples}");
        // 不是全静音
        let peak = wav[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).abs())
            .max()
            .unwrap_or(0);
        assert!(peak > 1000, "峰值 {peak}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_spx_to_wav(b"not an ogg file").is_err());
        assert!(decode_spx_to_wav(b"").is_err());
    }
}
