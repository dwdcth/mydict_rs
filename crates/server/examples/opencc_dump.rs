//! OpenCC 差分测试辅助工具（M0 门禁）。
//!
//! 从 stdin 按行读取词语，对四个配置 [T2S, S2T, S2TW, S2HK] 输出 TSV：
//!   word<TAB>t2s结果<TAB>s2t结果<TAB>s2tw结果<TAB>s2hk结果
//!
//! 转换失败时该列输出空串。用法：
//!   cargo run -p server --example opencc_dump < words.txt

use std::io::{self, BufRead, Write};

use opencc_rs::{Config, OpenCC};

fn main() {
    // 预热失败直接报错退出：连字典都加载不出来说明构建有问题，无需继续。
    let t2s = OpenCC::new([Config::T2S]).expect("init OpenCC t2s");
    let s2t = OpenCC::new([Config::S2T]).expect("init OpenCC s2t");
    let s2tw = OpenCC::new([Config::S2TW]).expect("init OpenCC s2tw");
    let s2hk = OpenCC::new([Config::S2HK]).expect("init OpenCC s2hk");

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("opencc_dump: read stdin failed: {e}");
                break;
            }
        };
        // 去掉行尾换行；保留词内空白与全角字符原样。
        let word = line.trim_end_matches(['\r', '\n']);

        let conv = |cc: &OpenCC, w: &str| -> String { cc.convert(w).unwrap_or_default() };

        let row = [
            word.to_string(),
            conv(&t2s, word),
            conv(&s2t, word),
            conv(&s2tw, word),
            conv(&s2hk, word),
        ];
        let _ = writeln!(out, "{}", row.join("\t"));
    }

    let _ = out.flush();
}
