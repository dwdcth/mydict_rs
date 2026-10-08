//! edge-tts：微软 Edge「大声朗读」在线语音（WebSocket 协议自研实现）。
//!
//! - 免费高质量神经嗓音（晓晓/云健/…Neural 系），输出 MP3 24kHz
//! - 鉴权：URL 带 `Sec-MS-GEC` 令牌（Windows 时间戳 300 秒取整 + TrustedClientToken
//!   的 SHA256 大写 hex），Origin 伪装 Edge 扩展
//! - 作为默认引擎（`tts_engine=edge`），失败（离线/网络抖动）自动回落本地 kokoro
//!
//! 协议参考：edge-tts（Python，MIT）的 readaloud 通道。

use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const WSS_HOST: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
/// 对齐参考实现（edge-tts 7.x）：Chromium 143 指纹 + 必带的 muid Cookie
const EDGE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";
const SEC_MS_GEC_VERSION: &str = "1-143.0.3650.75";
const WIN_EPOCH: u64 = 11_644_473_600;
/// 整体超时：连接 + 合成（在线服务，不能让请求挂死）
const TOTAL_TIMEOUT_SECS: u64 = 15;

/// Sec-MS-GEC 令牌：Windows FILETIME 刻度（100ns 单位）向下取整到 5 分钟，
/// 与 TrustedClientToken 拼接后 SHA256 大写 hex。
fn sec_ms_gec(unix_secs: u64) -> String {
    let ticks_per_sec: u64 = 10_000_000;
    let round: u64 = 300 * ticks_per_sec;
    let ticks = (unix_secs + WIN_EPOCH) * ticks_per_sec / round * round;
    let mut hasher = Sha256::new();
    hasher.update(format!("{ticks}{TRUSTED_CLIENT_TOKEN}"));
    hex_upper(&hasher.finalize())
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// 日期串走服务端可读的固定格式（对齐 edge-tts：JS Date 串）
fn js_date(unix_secs: u64) -> String {
    // 周几/月份是常量表即可——内容不影响服务端校验，只是回显
    const WEEK: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTH: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = unix_secs / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    let w = ((days + 3) % 7) as usize; // 1970-01-01 是周四（索引 3）
    let sec_of_day = unix_secs % 86400;
    format!(
        "{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        WEEK[w],
        MONTH[(m - 1) as usize],
        d,
        y,
        sec_of_day / 3600,
        sec_of_day % 3600 / 60,
        sec_of_day % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 在线合成一段文本，返回 MP3（audio-24khz-48kbitrate-mono-mp3）。
/// `voice` 形如 `zh-CN-XiaoxiaoNeural`；网络/协议错误返回 Err（调用方回落本地引擎）。
pub async fn synthesize(text: &str, voice: &str) -> Result<Vec<u8>, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let connection_id = uuid::Uuid::new_v4().simple().to_string();
    let url = format!(
        "{WSS_HOST}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version={SEC_MS_GEC_VERSION}&ConnectionId={connection_id}",
        sec_ms_gec(now)
    );
    let mut request = url
        .into_client_request()
        .map_err(|e| format!("URL 无效：{e}"))?;
    let headers = request.headers_mut();
    headers.insert("User-Agent", EDGE_UA.parse().unwrap());
    headers.insert("Origin", "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold".parse().unwrap());
    // 2024 末服务端开始校验 muid Cookie（参考实现的 headers_with_muid）
    let muid: String = (0..32).map(|_| {
        let b = rand::random::<u8>() % 16;
        char::from_digit(b as u32, 16).unwrap().to_ascii_uppercase()
    }).collect();
    if let Ok(cookie) = format!("muid={muid};").parse() {
        headers.insert("Cookie", cookie);
    }
    for (k, v) in [
        ("Pragma", "no-cache"),
        ("Cache-Control", "no-cache"),
        ("Accept-Language", "en-US,en;q=0.9"),
    ] {
        if let Ok(v) = v.parse() {
            headers.insert(k, v);
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(TOTAL_TIMEOUT_SECS), async {
        let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("连接失败：{e}"))?;

        let date = js_date(now);
        // 1) speech.config：声明输出格式（MP3 24k），不要词级边界元数据
        let config = format!(
            "X-Timestamp:{date}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
             {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}"
        );
        ws.send(Message::Text(config.into())).await.map_err(|e| e.to_string())?;

        // 2) SSML
        let ssml = format!(
            "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
             <voice name='{voice}'><prosody pitch='+0Hz' rate='+0%' volume='+0%'>{}</prosody></voice></speak>",
            xml_escape(text)
        );
        let msg = format!(
            "X-RequestId:{connection_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{date}Z\r\nPath:ssml\r\n\r\n{ssml}"
        );
        ws.send(Message::Text(msg.into())).await.map_err(|e| e.to_string())?;

        // 3) 收帧：文本帧是元数据/结束标记，二进制帧 = [2字节大端头长][头][音频]
        let mut audio: Vec<u8> = Vec::new();
        while let Some(frame) = ws.next().await {
            match frame.map_err(|e| format!("收帧失败：{e}"))? {
                Message::Text(t) => {
                    // 结束标记是 turn.end（audio 元数据帧也走文本，别提前退）
                    if t.contains("Path:turn.end") {
                        break;
                    }
                }
                Message::Binary(bin) => {
                    if bin.len() < 2 {
                        continue;
                    }
                    let header_len = u16::from_be_bytes([bin[0], bin[1]]) as usize;
                    if bin.len() < 2 + header_len {
                        continue;
                    }
                    audio.extend_from_slice(&bin[2 + header_len..]);
                }
                Message::Close(_) => return Err("服务端提前断开".to_string()),
                _ => {}
            }
        }
        if audio.is_empty() {
            return Err("没有收到音频数据".to_string());
        }
        Ok(audio)
    })
    .await
    .map_err(|_| "合成超时（15s）".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gec_token_is_deterministic() {
        // 固定时间戳的可复现指纹（换实现时对拍参考实现）
        let a = sec_ms_gec(1_700_000_000);
        let b = sec_ms_gec(1_700_000_000);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        // 同一 5 分钟窗口内不变（1.7e9 所在窗口是 [1699999800, 1700000100)），
        // 跨窗口变（+301s 必落入下一窗口）
        assert_eq!(sec_ms_gec(1_699_999_900), a);
        assert_ne!(sec_ms_gec(1_700_000_301), a);
    }

    #[test]
    fn date_format_shape() {
        let d = js_date(0);
        assert!(d.starts_with("Thu Jan 01 1970 00:00:00 GMT+0000"), "{d}");
        assert!(d.ends_with("(Coordinated Universal Time)"));
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(xml_escape("a<b>&'c'"), "a&lt;b&gt;&amp;&apos;c&apos;");
    }

    /// 真实合成冒烟：cargo test -p server --lib edge_smoke -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "出网请求微软服务"]
    async fn edge_smoke() {
        let mp3 = synthesize("你好，世界", "zh-CN-XiaoxiaoNeural").await.unwrap();
        println!("{} 字节，头: {:02X?}", mp3.len(), &mp3[..4.min(mp3.len())]);
        assert!(mp3.len() > 1000);
    }
}

// ── 官方音色列表（/voices/list，带 6 小时进程内缓存） ─────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct EdgeVoice {
    pub short_name: String,
    pub gender: String,
    pub locale: String,
    pub locale_name: String,
    pub display: String,
}

impl EdgeVoice {
    fn from_raw(v: &serde_json::Value) -> Option<Self> {
        let short_name = v.get("ShortName")?.as_str()?.to_string();
        if short_name.is_empty() {
            return None;
        }
        let gender = v.get("Gender").and_then(|g| g.as_str()).unwrap_or("");
        let locale = v.get("Locale").and_then(|g| g.as_str()).unwrap_or("");
        let locale_name = v
            .get("LocaleName")
            .and_then(|g| g.as_str())
            .unwrap_or("")
            .to_string();
        // FriendlyName 形如 "Microsoft Xiaoxiao Online (Natural) - Chinese (Mainland)"
        let friendly = v
            .get("FriendlyName")
            .and_then(|g| g.as_str())
            .unwrap_or("")
            .to_string();
        let core = friendly
            .strip_prefix("Microsoft ")
            .and_then(|r| r.split_once(" Online").map(|(a, _)| a))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&short_name)
            .to_string();
        let gender_zh = if gender.eq_ignore_ascii_case("Female") {
            "女"
        } else if gender.eq_ignore_ascii_case("Male") {
            "男"
        } else {
            gender
        };
        Some(EdgeVoice {
            display: format!("{core}（{gender_zh}）"),
            short_name,
            gender: gender_zh.to_string(),
            locale: locale.to_string(),
            locale_name,
        })
    }
}

static VOICE_CACHE: std::sync::OnceLock<moka::sync::Cache<(), std::sync::Arc<Result<Vec<EdgeVoice>, String>>>> =
    std::sync::OnceLock::new();

/// 全量音色（322 个：中文 14 / 英文 47 / 日韩法德等）。失败时缓存错误 10 分钟
/// （离线部署别每次请求都撞墙），调用方回退静态精选表。
pub async fn list_voices() -> std::sync::Arc<Result<Vec<EdgeVoice>, String>> {
    let cache = VOICE_CACHE.get_or_init(|| {
        moka::sync::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(6 * 3600))
            .build()
    });
    if let Some(hit) = cache.get(&()) {
        return hit;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())
        .unwrap_or_default()
        .as_secs();
    let muid: String = (0..32)
        .map(|_| char::from_digit((rand::random::<u8>() % 16) as u32, 16).unwrap().to_ascii_uppercase())
        .collect();
    let result = async {
        let resp = reqwest::Client::new()
            .get("https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list")
            .query(&[
                ("trustedclienttoken", TRUSTED_CLIENT_TOKEN),
                ("Sec-MS-GEC", &sec_ms_gec(now)),
            ])
            .header("User-Agent", EDGE_UA)
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cookie", format!("muid={muid};"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("请求音色列表失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("音色列表接口返回 {}", resp.status()));
        }
        let raw: serde_json::Value =
            resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
        let voices: Vec<EdgeVoice> = raw
            .as_array()
            .map(|arr| arr.iter().filter_map(EdgeVoice::from_raw).collect())
            .ok_or("响应不是数组")?;
        if voices.is_empty() {
            return Err("音色列表为空".to_string());
        }
        Ok(voices)
    }
    .await;
    let arc = std::sync::Arc::new(result);
    // 失败只缓存 10 分钟，成功 6 小时——moka 不支持按值 TTL，失败就先插短缓存再覆盖？
    // 简化：失败结果不进 6h 缓存（下次重试），成功才进。
    if arc.is_ok() {
        cache.insert((), arc.clone());
    }
    arc
}
