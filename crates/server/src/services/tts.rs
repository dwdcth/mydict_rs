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
/// 音频内容版本（缓存键 + 前端 URL 参数）：改合成参数（如语速修正）后递增，
/// 避免旧缓存继续分发修复前的音频
const AUDIO_REV: u32 = 9;
/// kokoro-micro 把「用户语速」乘 `SPEED_SCALE=0.65` 当模型语速。模型语速 0.65
/// 超出 Kokoro 时长预测器的训练分布：**首音节会被拉成连读两遍**（Whisper 实测
/// 「你好」→「您-也-好」三个音节、参考实现模型语速 1.0 → 干净两个音节）。
/// 这里传 1/0.65 把模型语速归一回参考实现的 1.0。
const SPEED_UNDO: f32 = 1.0 / 0.65;
/// 孤立单字的额外提速。曾试 1.2：实测会**放大**部分嗓音（晓妮）孤立三声的
/// 升调尾（「好」→「好嘞」），1.0 干净——提速对单字弊大于利，归一不提。
const SINGLE_CHAR_SPEEDUP: f32 = 1.0;

/// 词是不是恰好一个汉字（标点/空白不算）
fn is_single_hanzi(word: &str) -> bool {
    let mut han = 0;
    let mut other = 0;
    for c in word.chars() {
        if has_cjk(&c.to_string()) {
            han += 1;
        } else if !c.is_whitespace() {
            other += 1;
        }
    }
    han == 1 && other == 0
}

/// 收紧孤立单字的尾巴：能量降到峰值 65% 以下就收（40ms 淡出 + 20ms 余量）。
/// 模型对孤立三声的升调余韵会被听成一个「yi」音节（用户与 Whisper 一致报告），
/// 45% 阈值不够狠——65% 直接切在余韵起来之前，留下的是半三声（只降不升），
/// 词典报字音场景可接受；干净音节（四声快衰减）只损失自然收尾的一点点。
fn tighten_tail(samples: &mut [f32]) -> usize {
    const WIN: usize = 480; // 20ms @24k
    const TAIL_RATIO: f32 = 0.65;
    if samples.len() < WIN * 4 {
        return samples.len();
    }
    let n_windows = samples.len() / WIN;
    let mut peak: f32 = 0.0;
    let mut rms = vec![0f32; n_windows];
    for (w, slot) in rms.iter_mut().enumerate() {
        let mut sum = 0.0;
        for &v in &samples[w * WIN..(w + 1) * WIN] {
            sum += v * v;
        }
        *slot = (sum / WIN as f32).sqrt();
        peak = peak.max(*slot);
    }
    let threshold = peak * TAIL_RATIO;
    let Some(last_loud) = rms.iter().rposition(|v| *v > threshold) else {
        return samples.len();
    };
    let cut_end = ((last_loud + 1) * WIN + WIN).min(samples.len());
    fade_out(samples, cut_end);
    cut_end
}

/// 末尾 40ms 线性淡出（在 cut_end 处收笔）
fn fade_out(samples: &mut [f32], cut_end: usize) {
    let fade_from = cut_end.saturating_sub(960);
    for i in fade_from..cut_end {
        let t = (i - fade_from) as f32 / (cut_end - fade_from).max(1) as f32;
        samples[i] *= 1.0 - t;
    }
}

/// 在**基频谷底**收笔（孤立单字专用）：三声的升调段能量与主体几乎一样高
/// （实测峰值 75-98%），能量阈值切不掉、却正是被听成「yi」的元凶——
/// 那段根本不是 /i/ 元音（F2 全程低于 F1×1.5），纯粹是基频上挑的听感。
/// 谷底后 100ms 内基频回升 ≥ 40Hz 视为「有升段」，在谷底 +20ms 处淡出；
/// 四声（基频一路降到底）与一声（平）找不到升段 → 原样返回，交给能量兜底。
fn cut_at_pitch_bottom(samples: &mut [f32]) -> usize {
    const WIN: usize = 600; // 25ms @24k
    const HOP: usize = 240; // 10ms
    const MIN_LAG: usize = 60; // 400Hz
    const MAX_LAG: usize = 343; // 70Hz
    let n_frames = samples.len().saturating_sub(WIN) / HOP;
    if n_frames < 8 {
        return samples.len();
    }
    let mut peak_rms = 0f32;
    let mut frames: Vec<(f32, f32)> = Vec::with_capacity(n_frames); // (rms, f0)
    for f in 0..n_frames {
        let start = f * HOP;
        let seg = &samples[start..start + WIN];
        let mean = seg.iter().sum::<f32>() / WIN as f32;
        let centered: Vec<f32> = seg.iter().map(|v| v - mean).collect();
        let mut energy = 0.0;
        for &v in &centered {
            energy += v * v;
        }
        let rms = (energy / WIN as f32).sqrt();
        peak_rms = peak_rms.max(rms);
        let mut f0 = 0f32;
        if rms > 0.05 {
            let mut best_r = 0f32;
            let mut best_lag = 0usize;
            for lag in MIN_LAG..MAX_LAG {
                let m = WIN - lag;
                let mut num = 0.0;
                let mut e1 = 0.0;
                let mut e2 = 0.0;
                for j in 0..m {
                    num += centered[j] * centered[j + lag];
                    e1 += centered[j] * centered[j];
                    e2 += centered[j + lag] * centered[j + lag];
                }
                let den = (e1 * e2).sqrt();
                if den > 0.0 {
                    let r = num / den;
                    if r > best_r {
                        best_r = r;
                        best_lag = lag;
                    }
                }
            }
            // 归一回 120-350Hz 区间（八度误差修正）
            if best_r > 0.35 && best_lag > 0 {
                let mut f = 24000.0 / best_lag as f32;
                while f > 350.0 {
                    f /= 2.0;
                }
                while f < 120.0 {
                    f *= 2.0;
                }
                f0 = f;
            }
        }
        frames.push((rms, f0));
    }
    // 只看后 60% 的有声帧里基频最低的那帧
    let voiced_threshold = peak_rms * 0.25;
    let from = n_frames * 2 / 5;
    let mut bottom: Option<(usize, f32)> = None;
    for (idx, &(rms, f0)) in frames.iter().enumerate() {
        if idx < from || rms < voiced_threshold || f0 <= 0.0 {
            continue;
        }
        if bottom.is_none_or(|(_, b)| f0 < b) {
            bottom = Some((idx, f0));
        }
    }
    let Some((bottom_idx, bottom_f0)) = bottom else {
        return samples.len();
    };
    // 谷底后 100ms（10 帧）内回升 ≥ 40Hz → 确认是三声升段
    let has_rise = frames[(bottom_idx + 1)..n_frames.min(bottom_idx + 11)]
        .iter()
        .any(|&(rms, f0)| rms >= voiced_threshold && f0 >= bottom_f0 + 40.0);
    if !has_rise {
        return samples.len();
    }
    // 谷底帧就是吱呀+升段的起点，从那里收笔（进入谷底帧 5ms 即淡出），
    // 保留的是「高降」半三声——词典报字音的紧凑形式。
    // 保底：吱呀段的 F0 八度误检会把谷底定位得过早、把元音切掉（实测「水」
    // 被截到 0.17s）——裁剪结果短于 0.22s 或不足原长 55% 时弃用本方法
    let cut_end = (bottom_idx * HOP + 120).min(samples.len());
    if cut_end < 24000 * 22 / 100 || (cut_end as f32) < samples.len() as f32 * 0.55 {
        return samples.len();
    }
    fade_out(samples, cut_end);
    cut_end
}

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

/// 词里是否含 CJK 字符（选中文嗓音的依据）。
/// 含 PUA 私有区与 CJK 扩展区：说文系字头是 PUA 字形（U+F59A3 等），扩展 B+
/// 的生僻字同理——它们只可能走「词典注音 → 普通话拼音」兜底，必须选中文嗓音
/// （英文嗓音读中文音素会把 jin 扭成 jian 一类的怪音）。
fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
            || ('\u{F900}'..='\u{FAFF}').contains(&c)
            || ('\u{20000}'..='\u{3FFFF}').contains(&c)
            || ('\u{E000}'..='\u{F8FF}').contains(&c)
            || ('\u{F0000}'..='\u{FFFFD}').contains(&c)
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

/// 音素串里有没有「模型会读出声」的东西：IPA 都在 U+E000 以下，而 PUA 私有区
/// （说文系字头字形）与 astral 扩展区字符会原样混进音素串、到 tokenize 才被丢掉。
fn has_spoken_phonemes(ps: &str) -> bool {
    ps.chars().any(|c| {
        !c.is_whitespace() && !c.is_ascii_punctuation() && (c as u32) < 0xE000
    })
}

/// 注音提取规则（管理后台可配，存 system_settings 的 tts_pinyin_rules）。
/// `pattern` 用 fancy-regex 语法（支持前后看断言）；**第一个捕获组**是拼音，
/// 没有捕获组时取整体匹配。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PinyinRule {
    pub name: String,
    pub pattern: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// 解析并校验规则 JSON（数组，或存库的 JSON 字符串）。
/// 上限与长度约束防滥用；正则逐条编译，坏了一条就整体拒绝并指名道姓。
pub fn parse_pinyin_rules(value: &serde_json::Value) -> Result<Vec<PinyinRule>, AppError> {
    let rules: Vec<PinyinRule> = match value {
        serde_json::Value::String(raw) if raw.trim().is_empty() => return Ok(Vec::new()),
        serde_json::Value::String(raw) => serde_json::from_str(raw)
            .map_err(|e| AppError::validation(&format!("tts_pinyin_rules 不是合法 JSON：{e}")))?,
        serde_json::Value::Array(_) => serde_json::from_value(value.clone())
            .map_err(|e| AppError::validation(&format!("tts_pinyin_rules 结构不对：{e}")))?,
        serde_json::Value::Null => return Ok(Vec::new()),
        other => {
            return Err(AppError::validation(&format!(
                "tts_pinyin_rules 应为数组，收到 {other:?}"
            )))
        }
    };
    if rules.len() > 32 {
        return Err(AppError::validation("注音提取规则最多 32 条"));
    }
    for (idx, rule) in rules.iter().enumerate() {
        let label = if rule.name.trim().is_empty() {
            format!("第 {} 条", idx + 1)
        } else {
            format!("「{}」", rule.name.trim())
        };
        if rule.name.chars().count() > 40 {
            return Err(AppError::validation(&format!("{label} 名称超过 40 字")));
        }
        if rule.pattern.trim().is_empty() {
            return Err(AppError::validation(&format!("{label} 正则为空")));
        }
        if rule.pattern.chars().count() > 500 {
            return Err(AppError::validation(&format!("{label} 正则超过 500 字")));
        }
        if let Err(e) = fancy_regex::Regex::new(&rule.pattern) {
            return Err(AppError::validation(&format!("{label} 正则编译失败：{e}")));
        }
    }
    Ok(rules)
}

/// 用一条编译好的规则从文本里提注音：第一个捕获组，无捕获组取整体匹配
fn extract_rule(rule: &fancy_regex::Regex, text: &str) -> Option<String> {
    let caps = rule.captures(text).ok().flatten()?;
    let group = caps
        .get(1)
        .or_else(|| caps.get(0))?;
    let value = group.as_str().trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// 拼音像不像拼音（提取结果的守门员：长度 + 字符白名单）
fn plausible_pinyin(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.len() <= 30
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || "üvāáǎàēéěèīíǐìōóǒòūúǔùǖǘǚǜńňǹḿ' -".contains(c)
        })
}

/// 读取管理后台配置的规则（坏 JSON 静默降级为空——朗读不能因为配置损坏而 500）
async fn load_pinyin_rules(db: &DatabaseConnection) -> Vec<PinyinRule> {
    let raw = crate::services::settings_service::get_setting(db, "tts_pinyin_rules", Some(""))
        .await
        .unwrap_or_default()
        .unwrap_or_default();
    let value = serde_json::Value::String(raw);
    parse_pinyin_rules(&value).unwrap_or_default()
}

/// 从词典里找这个词的注音（PUA 生僻字等无法字面转写时的 TTS 兜底）：
/// 1) `dict_entries.phonetic` 列（长得像拼音才用）
/// 2) **管理后台配置的正则规则**（按序，第一个捕获组 = 拼音）
/// 3) 内置兜底：`<py>jīn</py>`（说文系）/ `class="py">yù<`（古今系）
/// 词头本身可能是 `@@@LINK=兓兓` 这种链接词条（PUA 字形 → 正字），跟随最多 5 跳。
async fn find_pronunciation_hint(db: &DatabaseConnection, word: &str) -> Option<String> {
    let plausible = |s: &str| -> bool { plausible_pinyin(s) };

    let mut current = word.trim().to_string();
    for _ in 0..5 {
        let rows = db
            .query_all_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT phonetic, definition FROM dict_entries WHERE word_lower = $1 LIMIT 8",
                [current.to_lowercase().into()],
            ))
            .await
            .ok()?;

        for row in &rows {
            if let Ok(p) = row.try_get::<String>("", "phonetic") {
                if plausible(&p) {
                    return Some(p.trim().to_string());
                }
            }
        }
        let custom_rules = load_pinyin_rules(db).await;
        for row in &rows {
            let Ok(def) = row.try_get::<String>("", "definition") else {
                continue;
            };
            for rule in &custom_rules {
                if !rule.enabled {
                    continue;
                }
                let Ok(compiled) = fancy_regex::Regex::new(&rule.pattern) else {
                    continue;
                };
                if let Some(p) = extract_rule(&compiled, &def) {
                    if plausible(&p) {
                        return Some(p);
                    }
                }
            }
            // <py>…</py>（说文系）
            if let Some(start) = def.find("<py>") {
                let rest = &def[start + 4..];
                if let Some(end) = rest.find("</py>") {
                    let p = rest[..end].trim();
                    if plausible(p) {
                        return Some(p.to_string());
                    }
                }
            }
            // class="py">…<（古今系）
            if let Some(start) = def.find("class=\"py\"") {
                let rest = &def[start..];
                if let Some(open) = rest.find('>') {
                    let inner = &rest[open + 1..];
                    let end = inner.find('<').unwrap_or(inner.len().min(30));
                    let p = inner[..end].trim();
                    if plausible(p) {
                        return Some(p.to_string());
                    }
                }
            }
        }
        // 没找到注音：跟随 @@@LINK 换目标再找
        let next = rows.iter().find_map(|row| {
            let def = row.try_get::<String>("", "definition").ok()?;
            let target = def.trim().strip_prefix("@@@LINK=")?.trim();
            let target = target.split_whitespace().next()?.trim();
            (!target.is_empty()).then(|| target.to_string())
        })?;
        if next == current {
            return None;
        }
        current = next;
    }
    None
}

/// 合成并返回 WAV 字节（24kHz mono 16-bit）
pub async fn synthesize_wav(
    state: &AppState,
    word: &str,
    lang_hint: Option<&str>,
) -> Result<Arc<Vec<u8>>, AppError> {
    synthesize_wav_with_voice(state, word, lang_hint, None).await
}

/// voice_override：试听指定嗓音（None = 按语言自动选设置里的嗓音）
pub async fn synthesize_wav_with_voice(
    state: &AppState,
    word: &str,
    lang_hint: Option<&str>,
    voice_override: Option<&str>,
) -> Result<Arc<Vec<u8>>, AppError> {
    let word = word.trim();
    if word.is_empty() || word.chars().count() > 500 {
        return Err(AppError::validation("文本为空或超过 500 字"));
    }
    let (enabled, zh_voice, en_voice) = tts_settings(&state.db).await?;
    if !enabled {
        return Err(AppError::internal_msg(
            "TTS 未启用（管理后台 → 系统设置里开启）",
        ));
    }
    let voice = match voice_override {
        Some(v) if (2..=32).contains(&v.len()) && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            v.to_string()
        }
        _ => pick_voice(word, lang_hint, &zh_voice, &en_voice),
    };

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

    let text = word.to_string();

    // 字面转写不出读音（PUA 生僻字、表意描述符等）→ 用词典注音兜底。
    // 探测用「自然语言」嗓音（管理员试听指定的嗓音不影响判定）；
    // 兜底出来的必然是普通话拼音，合成必须用中文嗓音——英文嗓音读中文
    // 音素会把 jin 扭成 jian 一类的怪音。缓存键用最终嗓音，放在定案之后。
    // 注意：std Mutex guard 不能跨 await，探测锁在块内放下。
    let literal_ok = {
        let probe_voice = pick_voice(word, lang_hint, &zh_voice, &en_voice);
        let guard = engine
            .lock()
            .map_err(|_| AppError::internal_msg("TTS 引擎锁中毒"))?;
        guard
            .phonemize(&text, Some(&probe_voice))
            .map(|ps| has_spoken_phonemes(&ps))
            .unwrap_or(false)
    };
    let pinyin_fallback = if literal_ok {
        None
    } else {
        match find_pronunciation_hint(&state.db, word).await {
            Some(p) => Some(p),
            None => {
                return Err(AppError::validation(
                    "这个词读不出来：字库里没有它的读音，词典里也没找到注音",
                ))
            }
        }
    };
    let voice = if pinyin_fallback.is_some() {
        zh_voice
    } else {
        voice
    };

    let cache_key = format!("{AUDIO_REV}|{voice}|{word}");
    if let Some(hit) = state.tts.audio.get(&cache_key) {
        return Ok(hit);
    }

    // CPU 密集：单并发信号量 + 阻塞线程
    let _permit = state
        .tts
        .permit
        .acquire()
        .await
        .map_err(|e| AppError::internal("tts-semaphore", e))?;

    let engine_for_task = engine.clone();
    let single_char = is_single_hanzi(word);
    let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, String> {
        let engine_guard = engine_for_task
            .lock()
            .map_err(|_| "TTS 引擎锁中毒".to_string())?;
        let speed = if single_char {
            SPEED_UNDO * SINGLE_CHAR_SPEEDUP
        } else {
            SPEED_UNDO
        };
        match pinyin_fallback {
            Some(pinyin) => engine_guard.synthesize_pinyin(&pinyin, Some(&voice), speed, 1.0),
            None => engine_guard.synthesize_with_options(&text, Some(&voice), speed, 1.0, None),
        }
    })
    .await
    .map_err(|e| AppError::internal("tts-join", e))?
    .map_err(|e| AppError::internal_msg(&format!("合成失败：{e}")))?;

    // 裁掉首尾静音：kokoro 输出常带 0.4s 头部 + 1s+ 尾部死寂，整句朗读时
    // 「说完隔一秒又来一段」的听感就是它造成的
    let mut trimmed = trim_silence(&samples);
    let final_len = if single_char {
        // 基频谷底法（三声升段 = 「yi」听感的元凶，能量切不掉）与能量法
        // 各算各的，取更严的那个——谷底检测偏晚时能量法兜住，反之亦然
        let mut by_pitch = trimmed.clone();
        let pitch_cut = cut_at_pitch_bottom(&mut by_pitch);
        let mut by_energy = trimmed.clone();
        let energy_cut = tighten_tail(&mut by_energy);
        if pitch_cut <= energy_cut {
            trimmed = by_pitch;
            pitch_cut
        } else {
            trimmed = by_energy;
            energy_cut
        }
    } else {
        trimmed.len()
    };
    let wav = Arc::new(f32_to_wav(&trimmed[..final_len]));
    state.tts.audio.insert(cache_key, wav.clone());
    Ok(wav)
}

/// 按能量裁首尾静音（阈值 0.6% 满幅；前后各留 60ms 自然余韵）
fn trim_silence(samples: &[f32]) -> Vec<f32> {
    const THRESHOLD: f32 = 0.006;
    const MARGIN_FRAC: f32 = 0.06; // 60ms @24k
    let window = 120; // 5ms 能量窗
    let loud_at = |i: usize| -> bool {
        let end = (i + window).min(samples.len());
        samples[i..end].iter().any(|s| s.abs() > THRESHOLD)
    };
    let mut start = 0;
    while start + window < samples.len() && !loud_at(start) {
        start += window;
    }
    let mut end = samples.len();
    while end > start + window && !loud_at(end.saturating_sub(window)) {
        end -= window;
    }
    if start >= end || end - start < 2400 {
        return samples.to_vec(); // 全静音/过短：保底返回原文
    }
    let margin = (SAMPLE_RATE as f32 * MARGIN_FRAC) as usize;
    let from = start.saturating_sub(margin);
    let to = (end + margin).min(samples.len());
    samples[from..to].to_vec()
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
        // PUA 私有区字头（说文系）与扩展 B 生僻字 → 中文嗓音（走注音兜底）
        assert_eq!(pick_voice("\u{F59A3}\u{F59A3}", None, "zf_a", "af_b"), "zf_a");
        assert_eq!(pick_voice("\u{204C3}", None, "zf_a", "af_b"), "zf_a");
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

    #[test]
    fn pinyin_rules_parse_and_validate() {
        // 数组与 JSON 字符串两种形态都能解析
        let rules = parse_pinyin_rules(&serde_json::json!([
            {"name": "说文 py", "pattern": "<py>([^<]+)</py>"},
            {"name": "停用示例", "pattern": "x", "enabled": false},
        ]))
        .unwrap();
        assert_eq!(rules.len(), 2);
        assert!(rules[0].enabled);
        assert!(!rules[1].enabled);
        let as_str = parse_pinyin_rules(&serde_json::Value::String(
            r#"[{"name":"a","pattern":"b"}]"#.to_string(),
        ))
        .unwrap();
        assert_eq!(as_str.len(), 1);
        // 空串 / null = 无规则
        assert!(parse_pinyin_rules(&serde_json::Value::String(String::new())).unwrap().is_empty());
        assert!(parse_pinyin_rules(&serde_json::Value::Null).unwrap().is_empty());
        // 坏正则指名道姓
        let err = parse_pinyin_rules(&serde_json::json!([
            {"name": "坏的", "pattern": "(unclosed"},
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("坏的"), "{err}");
        // 条数上限
        let many: Vec<_> = (0..33)
            .map(|i| serde_json::json!({"name": format!("r{i}"), "pattern": "x"}))
            .collect();
        assert!(parse_pinyin_rules(&serde_json::Value::Array(many)).is_err());
    }

    #[test]
    fn rule_extraction_uses_first_capture_group() {
        let re = fancy_regex::Regex::new(r"<音>([a-züāáǎàēéěèīíǐìōóǒòūúǔù]+)</音>").unwrap();
        let html = "<span>字</span><音>jīn</音>";
        assert_eq!(extract_rule(&re, html).as_deref(), Some("jīn"));
        // 无捕获组 → 整体匹配
        let re2 = fancy_regex::Regex::new(r"[a-z]+īn").unwrap();
        assert_eq!(extract_rule(&re2, html).as_deref(), Some("jīn"));
        // 不命中
        let re3 = fancy_regex::Regex::new(r"<x>(.+)</x>").unwrap();
        assert_eq!(extract_rule(&re3, html), None);
    }

    #[test]
    fn single_hanzi_detection() {
        assert!(is_single_hanzi("好"));
        assert!(is_single_hanzi("兓"));
        assert!(is_single_hanzi("  豫 "));
        assert!(!is_single_hanzi("好奇"));
        assert!(!is_single_hanzi("好。"));
        assert!(!is_single_hanzi("apple"));
        assert!(!is_single_hanzi(""));
    }

    #[test]
    fn tighten_tail_cuts_weak_creak() {
        // 主体 0.3s 全幅 + 尾巴 0.3s 半幅吱呀：尾巴应被收紧到 ~45% 阈值处
        let mut samples = Vec::new();
        for i in 0..7200 {
            let t = i as f32 / 24000.0;
            let amp = if i < 7200 / 2 { 0.8 } else { 0.5 };
            samples.push(amp * (2.0 * std::f32::consts::PI * 220.0 * t).sin());
        }
        let cut = tighten_tail(&mut samples);
        // 主体（0.3s=7200 样本一半=3600）必须完整保留，尾巴（0.5 幅度 > 45%*0.8=0.36
        // 仍在）说明半幅尾巴高于阈值不会被砍——构造再低一点的尾巴验证会砍：
        assert!(cut >= 3600, "主体不能被裁：{cut}");
        let mut s2 = Vec::new();
        for i in 0..7200 {
            let t = i as f32 / 24000.0;
            let amp = if i < 3600 { 0.8 } else { 0.2 }; // 尾巴 20% < 45%
            s2.push(amp * (2.0 * std::f32::consts::PI * 220.0 * t).sin());
        }
        let cut2 = tighten_tail(&mut s2);
        assert!(cut2 < 4800, "低幅尾巴应被裁：{cut2}");
        assert!(cut2 >= 3600, "主体不能被裁：{cut2}");
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

#[cfg(test)]
mod voices_probe {
    #[test]
    #[ignore = "需要本地模型"]
    fn list_voices() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        let zh: Vec<String> = tts.voices().into_iter().filter(|v| v.starts_with('z')).collect();
        println!("中文嗓音: {:?}", zh);
    }
}

#[cfg(test)]
mod preview_gen {
    #[test]
    #[ignore = "生成中文嗓音试听样本到 /tmp/tts-previews"]
    fn gen_previews() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        let sentence = "豫章故郡，洪都新府。星分翼轸，地接衡庐。";
        std::fs::create_dir_all("/tmp/tts-previews").unwrap();
        for voice in tts.voices().into_iter().filter(|v| v.starts_with('z')) {
            let audio = tts
                .synthesize_with_options(sentence, Some(&voice), 1.0, 1.0, None)
                .unwrap();
            tts.save_wav(&format!("/tmp/tts-previews/{voice}.wav"), &audio).unwrap();
            println!("{voice} ok");
        }
    }
}

#[cfg(test)]
mod dup_probe {
    #[test]
    #[ignore = "需要本地模型"]
    fn probe_dup() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        for w in ["豫", "章", "豫章", "豫章故郡", "好奇", "apple"] {
            let p = tts.phonemize(w, Some("zf_xiaobei")).unwrap();
            println!("{w}  →  {p}");
        }
        // 用户报「两个豫」的完整试听句 + 分句对照
        let sentence = "豫章故郡，洪都新府。星分翼轸，地接衡庐。";
        println!("试听句 →  {}", tts.phonemize(sentence, Some("zf_xiaobei")).unwrap());
        for clause in ["豫章故郡", "洪都新府", "星分翼轸", "地接衡庐"] {
            println!("分句[{clause}] →  {}", tts.phonemize(clause, Some("zf_xiaobei")).unwrap());
        }
    }
}

#[cfg(test)]
mod path_equiv_probe {
    /// 两条链路同音素是否同音频：cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "需要本地模型"]
    fn probe_path_equivalence() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        let a = tts.synthesize_with_options("金", Some("zf_xiaobei"), 1.0 / 0.65, 1.0, None).unwrap();
        let b = tts.synthesize_pinyin("jīn", Some("zf_xiaobei"), 1.0 / 0.65, 1.0).unwrap();
        println!("字面路径: {} 样本, {:?}…", a.len(), &a[..4]);
        println!("拼音路径: {} 样本, {:?}…", b.len(), &b[..4]);
        println!("相同: {}", a == b);
        if a != b && a.len() == b.len() {
            let diff = a.iter().zip(&b).filter(|(x, y)| x != y).count();
            println!("差异样本数: {}/{}", diff, a.len());
        }
    }
}

#[cfg(test)]
mod single_char_speed_probe {
    /// 单字不同模型语速的时长/听感：cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "需要本地模型"]
    fn probe_speeds() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        for model_speed in [1.0f32, 1.1, 1.2, 1.3] {
            let user_speed = model_speed / 0.65; // 引擎内部再乘 0.65
            let a = tts.synthesize_with_options("好", Some("zf_xiaobei"), user_speed, 1.0, None).unwrap();
            println!("模型语速 {model_speed}: 好 = {:.2}s", a.len() as f32 / 24000.0);
            let _ = tts.save_wav(&format!("/tmp/hao_sp{model_speed}.wav"), &a);
        }
    }
}

#[cfg(test)]
mod single_char_tail_probe {
    /// 单字加标点对尾巴的影响：cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "需要本地模型"]
    fn probe_punct_tail() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        let mut idx = 0usize;
        for text in ["好", "好。", "好，", "好！", "五", "五。"] {
            idx += 1;
            let a = tts.synthesize_with_options(text, Some("zf_xiaobei"), 1.0 / 0.65 * 1.2, 1.0, None).unwrap();
            // 模拟服务端：trim_silence + 65% 收尾后的时长
            let trimmed = super::trim_silence(&a);
            let mut t = trimmed.clone();
            let cut = super::tighten_tail(&mut t);
            println!(
                "{text:>4}: 原始 {:.2}s / 裁静音 {:.2}s / 收尾后 {:.2}s",
                a.len() as f32 / 24000.0,
                trimmed.len() as f32 / 24000.0,
                cut as f32 / 24000.0
            );
            let _ = tts.save_wav(&format!("/tmp/tail_{idx}.wav"), &t[..cut]);
        }
    }
}

#[cfg(test)]
mod speed_voice_matrix_probe {
    use super::trim_silence;

    /// 速度×嗓音矩阵（临时）：cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "需要本地模型"]
    fn matrix() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let tts = rt.block_on(kokoro_micro::TtsEngine::new()).unwrap();
        for voice in ["zf_xiaobei", "zf_xiaoni", "zm_yunjian"] {
            for model_speed in [1.0f32, 1.2] {
                let tag = format!("{voice}_{model_speed}");
                let a = tts
                    .synthesize_with_options("好", Some(voice), model_speed / 0.65, 1.0, None)
                    .unwrap();
                let trimmed = trim_silence(&a);
                let _ = tts.save_wav(&format!("/tmp/mx_{tag}_raw.wav"), &a);
                let _ = tts.save_wav(&format!("/tmp/mx_{tag}_trim.wav"), &trimmed);
                println!(
                    "{tag}: raw {:.2}s trim {:.2}s",
                    a.len() as f32 / 24000.0,
                    trimmed.len() as f32 / 24000.0
                );
            }
        }
    }
}
