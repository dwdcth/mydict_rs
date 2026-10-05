//! Speex(.spx) → mp3 按需转码 —— 移植自 `app/services/spx_transcode.py`。
//!
//! 前端请求 x.mp3 未命中时服务端现转（speexdec → wav → lame），转好落盘下次直接命中。
//! 参数逐字对齐 Python：`--quiet -m m -b 32 --resample 22.05`（单声道 32kbps 22.05kHz）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Semaphore;

const TIMEOUT_SECONDS: u64 = 15;
/// 并发上限 2（对齐 Python BoundedSemaphore(2)）
static SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn semaphore() -> &'static Semaphore {
    SEMAPHORE.get_or_init(|| Semaphore::new(2))
}

/// speexdec 与 lame 是否都可用（缓存 which 结果；任一缺失则功能整体关闭）
pub fn tools_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        which("speexdec").is_some() && which("lame").is_some()
    })
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(tool);
        if candidate.is_file() {
            // 可执行位检查（Unix）
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&candidate) {
                    if meta.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// 把 source(.spx) 转成同名 .mp3（落源文件旁边），成功返回 mp3 路径。
/// 工具缺失/超时/失败返回 None；排队期间已被别的请求转好则直接复用。
pub async fn transcode_to_mp3(source: &Path) -> Option<PathBuf> {
    if !tools_available() {
        return None;
    }
    let target = source.with_extension("mp3");
    if target.is_file() {
        return Some(target);
    }
    let _permit = semaphore().acquire().await.ok()?;
    // 拿到信号量后再查一次：排队期间可能已被转好
    if target.is_file() {
        return Some(target);
    }
    let source = source.to_path_buf();
    let target2 = target.clone();
    let result = tokio::time::timeout(Duration::from_secs(TIMEOUT_SECONDS), async move {
        // 中转目录建在源文件父目录（.spx- 前缀），成功后 rename 原子落位
        let parent = source.parent()?;
        let work = parent.join(format!(
            ".spx-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&work).await.ok()?;
        let wav = work.join("out.wav");
        let mp3_tmp = work.join("out.mp3");
        async fn cleanup(work: &Path) {
            let _ = tokio::fs::remove_dir_all(work).await;
        }
        // speexdec input.spx out.wav
        let status = tokio::process::Command::new("speexdec")
            .arg(&source)
            .arg(&wav)
            .status()
            .await;
        if !matches!(status, Ok(s) if s.success()) {
            cleanup(&work).await;
            return None;
        }
        // lame --quiet -m m -b 32 --resample 22.05 wav mp3
        let status = tokio::process::Command::new("lame")
            .arg("--quiet")
            .arg("-m")
            .arg("m")
            .arg("-b")
            .arg("32")
            .arg("--resample")
            .arg("22.05")
            .arg(&wav)
            .arg(&mp3_tmp)
            .status()
            .await;
        if !matches!(status, Ok(s) if s.success()) {
            cleanup(&work).await;
            return None;
        }
        if tokio::fs::rename(&mp3_tmp, &target2).await.is_err() {
            cleanup(&work).await;
            return None;
        }
        cleanup(&work).await;
        Some(target2)
    })
    .await;
    result.ok().flatten()
}
