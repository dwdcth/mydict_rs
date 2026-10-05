//! 日志：stdout + 按大小轮转的文件 —— 移植自 `app/core/logging.py`。
//!
//! Python 用 RotatingFileHandler（5MB × 5 个备份）。tracing-appender 只有时间轮转，
//! 这里实现一个按大小轮转的 MakeWriter（~70 行）避免引入 flexi_logger。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const BACKUP_COUNT: u32 = 5;

struct RotatingFile {
    inner: Mutex<State>,
    path: PathBuf,
}

struct State {
    file: File,
    written: u64,
}

impl RotatingFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Mutex::new(State { file, written }),
            path,
        })
    }

    fn write_line(&self, buf: &[u8]) -> io::Result<()> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.written + buf.len() as u64 > MAX_BYTES {
            self.rotate(&mut state);
        }
        state.file.write_all(buf)?;
        state.file.write_all(b"\n")?;
        state.written += buf.len() as u64 + 1;
        Ok(())
    }

    /// Python RotatingFileHandler 语义：log → log.1 → … → log.{BACKUP_COUNT}，最老的删除
    fn rotate(&self, state: &mut State) {
        let _ = state.file.sync_all();
        for i in (1..BACKUP_COUNT).rev() {
            let from = self.path.with_extension(format!("log.{i}"));
            let to = self.path.with_extension(format!("log.{}", i + 1));
            let _ = std::fs::rename(&from, &to);
        }
        let first = self.path.with_extension("log.1");
        let _ = std::fs::rename(&self.path, &first);
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => {
                state.file = f;
                state.written = 0;
            }
            Err(_) => {
                // 重开失败就继续写旧句柄（磁盘满/权限问题），不丢日志
                state.written = 0;
            }
        }
    }
}

/// 同时写 stdout 与轮转文件的 writer（tracing 的 with_writer 用）
pub struct TeeWriter {
    file: Arc<RotatingFile>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        // 文件行单独走 write_line 以便正确计数与轮转
        let line = buf.strip_suffix(b"\n").unwrap_or(buf);
        self.file.write_line(line)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        std::io::stdout().flush()?;
        self.file.inner.lock().unwrap_or_else(|e| e.into_inner()).file.flush()
    }
}

/// 初始化 tracing：默认 INFO，RUST_LOG 可覆盖；双写 stdout 与 {log_dir}/mydict.log
pub fn configure_logging(log_dir: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let file = match RotatingFile::open(PathBuf::from(log_dir).join("mydict.log")) {
        Ok(f) => Some(Arc::new(f)),
        Err(err) => {
            eprintln!("无法打开日志文件 {log_dir}/mydict.log: {err}，仅输出到 stdout");
            None
        }
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);

    match file {
        Some(file) => {
            builder
                .with_writer(move || TeeWriter { file: file.clone() })
                .init();
        }
        None => {
            builder.init();
        }
    }
}
