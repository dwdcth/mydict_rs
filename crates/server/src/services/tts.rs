//! TTS（kokoro-micro 内嵌引擎，Kokoro-82M ONNX）：词条无词典语音时的兜底发音。
//!
//! - 引擎懒加载（首次请求才初始化；首次初始化会自动下载模型 ~337MB 到
//!   `~/.cache/k/`，部署时建议挂卷持久化）；初始化失败会「闩住」，重启后重试
//! - 合成 CPU 密集：信号量=1 串行 + spawn_blocking，不占异步运行时
//! - 每词×嗓音的结果（WAV 字节）进 128MB 缓存，重复播放零成本
//! - 嗓音选语言：词里有 CJK 或 lang 以 zh 开头 → 中文嗓音，否则英文嗓音
//!   （Kokoro 的语言由嗓音名决定，九语内建）

use std::sync::Arc;

use moka::sync::Cache as MokaCache;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

use crate::core::errors::AppError;
use crate::AppState;

/// Kokoro 模型输出采样率（24kHz mono）
const SAMPLE_RATE: u32 = 24000;
/// 合成结果缓存上限（按字节计重）
const TTS_CACHE_BYTES: usize = 128 * 1024 * 1024;

pub struct TtsState {
    /// 引擎（含失败闩：Err 为首次初始化的错误信息）。
    /// std::sync::Mutex：合成在 spawn_blocking 里持锁（tokio::Mutex 的 guard 跨不过边界）
    engine: tokio::sync::OnceCell<Result<Arc<std::sync::Mutex<kokoro_micro::TtsEngine>>, String>>,
    /// 初始化互斥（防并发重复下载）
    init_lock: tokio::sync::Mutex<()>,
    /// 合成串行信号量（CPU 密集，单并发足够且防内存翻倍）
    permit: tokio::sync::Semaphore,
    /// word|voice → WAV 字节
    audio: MokaCache<String, Arc<Vec<u8>>>,
}

impl TtsState {
    pub fn new() -> Self {
        Self {
            engine: tokio::sync::OnceCell::new(),
            init_lock: tokio::sync::Mutex::new(()),
            permit: tokio::sync::Semaphore::new(1),
            audio: MokaCache::builder()
                .max_capacity(TTS_CACHE_BYTES as u64)
                .weigher(|_k, v: &Arc<Vec<u8>>| v.len() as u32)
                .build(),
        }
    }
}

/// 词里是否含 CJK 字符（选中文嗓音的依据）
fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
            || ('\u{F900}'..='\u{FAFF}').contains(&c)
    })
}

/// 按词形/语言提示选嗓音（pub 供测试）
pub fn pick_voice(word: &str, lang_hint: Option<&str>, zh_voice: &str, en_voice: &str) -> String {
    let zh = has_cjk(word) || lang_hint.map(|l| l.starts_with("zh")).unwrap_or(false);
    if zh {
        zh_voice.to_string()
    } else {
        en_voice.to_string()
    }
}

async fn get_voice_setting(db: &DatabaseConnection, key: &str, default: &str) -> String {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT value FROM system_settings WHERE key = $1",
            [key.into()],
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String>("", "value").ok())
        .filter(|v| !v.trim().is_empty());
    row.unwrap_or_else(|| default.to_string())
}

async fn tts_settings(db: &DatabaseConnection) -> Result<(bool, String, String), AppError> {
    let enabled = crate::services::settings_service::get_bool_setting(db, "tts_enabled", false)
        .await
        .unwrap_or(false);
    let zh = get_voice_setting(db, "tts_voice_zh", "zf_xiaoni").await;
    let en = get_voice_setting(db, "tts_voice_en", "af_heart").await;
    Ok((enabled, zh, en))
}

/// 合成并返回 WAV 字节（24kHz mono 16-bit）
pub async fn synthesize_wav(
    state: &AppState,
    word: &str,
    lang_hint: Option<&str>,
) -> Result<Arc<Vec<u8>>, AppError> {
    let word = word.trim();
    if word.is_empty() || word.chars().count() > 120 {
        return Err(AppError::validation("word 无效"));
    }
    let (enabled, zh_voice, en_voice) = tts_settings(&state.db).await?;
    if !enabled {
        return Err(AppError::internal_msg(
            "TTS 未启用（管理后台 → 系统设置里开启）",
        ));
    }
    let voice = pick_voice(word, lang_hint, &zh_voice, &en_voice);
    let cache_key = format!("{voice}|{word}");
    if let Some(hit) = state.tts.audio.get(&cache_key) {
        return Ok(hit);
    }

    // 引擎懒加载（首次会下载模型；失败闩住直到重启）
    let engine = match state.tts.engine.get() {
        Some(Ok(engine)) => engine.clone(),
        Some(Err(msg)) => {
            return Err(AppError::internal_msg(&format!(
                "TTS 引擎不可用（初始化失败已闩住，重启服务后重试）：{msg}"
            )))
        }
        None => {
            let _guard = state.tts.init_lock.lock().await;
            // 双检：等锁期间别的请求可能已初始化好
            match state.tts.engine.get() {
                Some(Ok(engine)) => engine.clone(),
                Some(Err(msg)) => {
                    return Err(AppError::internal_msg(&format!(
                        "TTS 引擎不可用（初始化失败已闩住，重启服务后重试）：{msg}"
                    )))
                }
                None => {
                    // TtsEngine::new 是 async（模型不存在时内部异步下载 ~337MB）
                    match kokoro_micro::TtsEngine::new().await {
                        Ok(engine) => {
                            let arc = Arc::new(std::sync::Mutex::new(engine));
                            let _ = state.tts.engine.set(Ok(arc.clone()));
                            arc
                        }
                        Err(err) => {
                            let msg = format!("模型加载失败（首次使用需联网下载 ~337MB 到 ~/.cache/k/）：{err}");
                            let _ = state.tts.engine.set(Err(msg.clone()));
                            return Err(AppError::internal_msg(&msg));
                        }
                    }
                }
            }
        }
    };

    // CPU 密集：单并发信号量 + 阻塞线程
    let _permit = state
        .tts
        .permit
        .acquire()
        .await
        .map_err(|e| AppError::internal("tts-semaphore", e))?;
    let text = word.to_string();
    let engine_for_task = engine.clone();
    let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, String> {
        let engine_guard = engine_for_task
            .lock()
            .map_err(|_| "TTS 引擎锁中毒".to_string())?;
        engine_guard.synthesize_with_options(&text, Some(&voice), 1.0, 1.0, None)
    })
    .await
    .map_err(|e| AppError::internal("tts-join", e))?
    .map_err(|e| AppError::internal_msg(&format!("合成失败：{e}")))?;

    let wav = Arc::new(f32_to_wav(&samples));
    state.tts.audio.insert(cache_key, wav.clone());
    Ok(wav)
}

/// f32 样本（-1..1）→ WAV 字节（16-bit PCM mono，44 字节头）
pub fn f32_to_wav(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // 字节率
    out.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
    out.extend_from_slice(&16u16.to_le_bytes()); // 位深
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        out.extend_from_slice(&((clamped * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}

/// 从释义 HTML 里提取**第一个**可播放的词典音频 URL（与 iframe 引导脚本同口径）：
/// - `sound://path` → /dict-res/{id}/res/path（未改写的引用）
/// - 已改写的 `/dict-res/...` 或相对路径，扩展名为音频（.spx 换 .mp3，服务端现转）
pub fn first_audio_url(definition_html: &str, dictionary_id: i32) -> Option<String> {
    const AUDIO_EXTS: &[&str] = &[
        "mp3", "wav", "ogg", "oga", "opus", "m4a", "aac", "flac", "wma", "spx",
    ];
    let quote = dict_parser::resources::quote_path_pub;
    let strip_q = |v: &str| -> String {
        let mut v = v.split(['?', '#']).next().unwrap_or(v).to_string();
        if let Some(stripped) = v.strip_suffix(".spx") {
            v = format!("{stripped}.mp3"); // /dict-res 会按需把 .spx 转成 .mp3
        }
        v
    };
    // 1) sound:// 原始引用
    if let Some(pos) = definition_html.find("sound://") {
        let rest = &definition_html[pos + 8..];
        let end = rest
            .find(['"', '\'', ' ', '<', ')'])
            .unwrap_or(rest.len());
        let path = &rest[..end];
        if !path.is_empty() {
            return Some(format!("/dict-res/{dictionary_id}/res/{}", quote(path)));
        }
    }
    // 2) href/src 指向音频扩展
    for attr in ["href=\"", "src=\""] {
        let mut search_from = 0usize;
        while let Some(rel) = definition_html[search_from..].find(attr) {
            let start = search_from + rel + attr.len();
            let rest = &definition_html[start..];
            let Some(end) = rest.find('"') else { break };
            let value = &rest[..end];
            search_from = start + end;
            let lower = value.to_lowercase();
            let ext_ok = AUDIO_EXTS.iter().any(|ext| {
                lower.ends_with(&format!(".{ext}"))
                    || lower.contains(&format!(".{ext}?"))
                    || lower.contains(&format!(".{ext}#"))
            });
            if !ext_ok {
                continue;
            }
            let cleaned = strip_q(value);
            if cleaned.starts_with("/dict-res/") || cleaned.starts_with("http://")
                || cleaned.starts_with("https://")
            {
                return Some(cleaned);
            }
            // 相对路径 → 该词典的资源路由
            return Some(format!(
                "/dict-res/{dictionary_id}/res/{}",
                quote(cleaned.trim_start_matches('/'))
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_picks_by_cjk_and_hint() {
        assert_eq!(pick_voice("苹果", None, "zf_a", "af_b"), "zf_a");
        assert_eq!(pick_voice("apple", None, "zf_a", "af_b"), "af_b");
        // 纯拉丁词 + zh 提示 → 中文嗓音
        assert_eq!(pick_voice("apple", Some("zh-Hans"), "zf_a", "af_b"), "zf_a");
        // 中文词 + en 提示 → 词形优先（中文词用英文嗓音会嘟囔）
        assert_eq!(pick_voice("苹果", Some("en"), "zf_a", "af_b"), "zf_a");
    }

    #[test]
    fn wav_header_shape() {
        let wav = f32_to_wav(&[0.0, 0.5, -0.5]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 3 * 2);
        // 采样率 24k
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 24000);
    }

    #[test]
    fn extracts_first_audio_url() {
        // sound:// 原始引用
        let url = first_audio_url(r#"<a href="sound://us/apple.mp3">🔊</a>"#, 7);
        assert_eq!(url.as_deref(), Some("/dict-res/7/res/us/apple.mp3"));
        // 已改写的 /dict-res 路径
        let url = first_audio_url(r#"<audio src="/dict-res/7/res/a.mp3"></audio>"#, 7);
        assert_eq!(url.as_deref(), Some("/dict-res/7/res/a.mp3"));
        // 相对 .spx → 换 .mp3（服务端现转）
        let url = first_audio_url(r#"<a href="sound/us/x.spx">s</a>"#, 7);
        assert!(url.unwrap().ends_with("/us/x.mp3"));
        // 无音频
        assert_eq!(first_audio_url("<p>n. 苹果</p>", 7), None);
        // 图片不算
        assert_eq!(first_audio_url(r#"<img src="a.png">"#, 7), None);
    }
}

#[cfg(test)]
mod polyphone_probe {
    /// 多音字探针：`cargo test -p server --lib polyphone -- --nocapture`
    /// 看「好/行/长/重…」在不同词里的声调轮廓（→↗↓↘）是否正确变调
    #[test]
    #[ignore = "需要本地已下载 Kokoro 模型；cargo test -- --ignored 运行"]
    fn probe_polyphones() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).expect("engine");
        for (ch, words) in [
            ("好", ["好奇", "好看"]),
            ("行", ["银行", "行走"]),
            ("长", ["长大", "长城"]),
            ("重", ["重要", "重复"]),
            ("得", ["得到", "觉得"]),
            ("还", ["还有", "还钱"]),
            ("都", ["首都", "都是"]),
            ("了", ["了解", "好了"]),
        ] {
            println!("── {ch} ──");
            for w in words {
                match tts.phonemize(w, Some("zf_xiaoni")) {
                    Ok(p) => println!("  {w}: {p}"),
                    Err(e) => println!("  {w}: ERR {e}"),
                }
            }
        }
    }
}
