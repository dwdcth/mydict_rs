//! MDict 解析器 —— 移植自 `app/parsers/mdict.py`，双后端：
//! - v1.2 / v2.0 → mdictlib（惰性迭代、MDD 枚举）
//! - v3.0 → opendict-rs fork（位置访问 + MDD 枚举）
//!
//! 路由：先 probe 头部 `GeneratedByEngineVersion`；探测失败（头加密等）先试 mdictlib，
//! 打开或迭代遇 `Unsupported` 再回退 opendict（对齐计划里的回退策略）。

mod stylesheet;

pub use stylesheet::{expand_style_markers, is_compact_value, parse_stylesheet};

use std::collections::HashMap;
use std::path::PathBuf;

use mdictlib::{MddFile, MdxFile};

use crate::entry::{
    is_informative, is_informative_headword, spread_downsample, ParsedEntry, SAMPLE_SCAN_FACTOR,
};
use crate::Headword;
use crate::resources as res;
use crate::{ParseOpts, ParserError, Result};

use super::probe::{probe_family, MdictFamily};

/// 迭代控制信号：Continue 继续，Stop 提前结束（采样到量时用，不算错误）
enum Flow {
    Continue,
    Stop,
}

pub struct MdictParser {
    /// 打开代价高（大 MDX 词头索引加载要几秒到几十秒），跨 parse/sample 复用同一次打开
    opened: Option<Opened>,
}

struct Opened {
    /// 组里的全部 .mdx（常见 1 部；手工上传多部时按文件顺序拼接，
    /// base = 该文件首条的全局序号）
    files: Vec<OpenedFile>,
}

struct OpenedFile {
    backend: Backend,
    sheet: HashMap<String, (String, String)>,
    compact: bool,
    base: u64,
    len: u64,
}

impl Opened {
    fn total(&self) -> u64 {
        self.files.last().map(|f| f.base + f.len).unwrap_or(0)
    }

    /// 全局序号 → (所在文件, 文件内序号)
    fn locate(&self, ordinal: u64) -> Option<(&OpenedFile, u64)> {
        self.files
            .iter()
            .find(|f| ordinal >= f.base && ordinal < f.base + f.len)
            .map(|f| (f, ordinal - f.base))
    }
}

/// 打开 MDX 的完整路由（probe → mdictlib → Unsupported 回退 opendict）。
/// 提取为关联函数：并行解析的每个 worker 要各自打开独立句柄。
fn open_backend(mdx_path: &PathBuf) -> Result<OpenedFile> {
    match probe_family(mdx_path) {
        Some(MdictFamily::Opendict) => MdictParser::open_opendict(mdx_path),
        Some(MdictFamily::Mdictlib) | None => match MdictParser::open_mdictlib(mdx_path) {
            Ok(opened) => Ok(opened),
            Err(ParserError::Unsupported(reason)) => {
                // probe 失败的文件可能是 mdictlib 不认识的版本（如 v3 加密头）——回退
                tracing::info!(reason = %reason, "mdictlib 不支持，回退 opendict 后端");
                MdictParser::open_opendict(mdx_path)
            }
            Err(other) => Err(other),
        },
    }
}

enum Backend {
    Mdictlib {
        mdx: MdxFile,
    },
    Opendict {
        dict: opendict::mdict::MdictDictionary,
    },
}

impl MdictParser {
    pub fn new() -> Self {
        Self { opened: None }
    }

    fn split_files<'a>(files: &'a [PathBuf]) -> (Vec<&'a PathBuf>, Vec<&'a PathBuf>) {
        let mut mdx = Vec::new();
        let mut mdd = Vec::new();
        for p in files {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "mdx" => mdx.push(p),
                "mdd" => mdd.push(p),
                _ => {}
            }
        }
        (mdx, mdd)
    }

    /// 打开（或复用）MDX。probe 决定后端；探测失败先 mdictlib。
    fn ensure_opened(&mut self, mdx_paths: &[&PathBuf]) -> Result<&mut Opened> {
        if self.opened.is_some() {
            return Ok(self.opened.as_mut().expect("checked"));
        }
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }
        // 多 .mdx 组：全部打开，全局序号按文件顺序拼接（lite 的 source_ordinal
        // 依此寻址，definition_at 用 locate 还原到具体文件）
        let mut files = Vec::with_capacity(mdx_paths.len());
        let mut base = 0u64;
        for mdx_path in mdx_paths {
            let mut opened_file = open_backend(mdx_path)?;
            opened_file.len = match &opened_file.backend {
                Backend::Mdictlib { mdx, .. } => mdx.len(),
                Backend::Opendict { dict } => dict.entry_count() as u64,
            };
            opened_file.base = base;
            base += opened_file.len;
            files.push(opened_file);
        }
        self.opened = Some(Opened { files });
        Ok(self.opened.as_mut().expect("just set"))
    }

    fn open_mdictlib(mdx_path: &PathBuf) -> Result<OpenedFile> {
        let options = mdictlib::OpenOptions::default().with_limits(large_limits());
        let mdx = MdxFile::open_with_options(mdx_path, &options)
            .map_err(map_mdictlib_err(mdx_path))?;
        let header = mdx.header();
        // 键大小写不规范的老词典按不区分大小写兜底（对齐 Python is_compact）
        let attr_ci = |name: &str| -> Option<&str> {
            header
                .attribute(name)
                .or_else(|| {
                    header
                        .attributes()
                        .find(|(k, _)| k.eq_ignore_ascii_case(name))
                        .map(|(_, v)| v)
                })
        };
        let sheet = parse_stylesheet(attr_ci("StyleSheet").map(|v| v.to_string()).as_deref());
        let compact = is_compact_value(attr_ci("Compact"));
        Ok(OpenedFile {
            backend: Backend::Mdictlib { mdx },
            sheet,
            compact,
            base: 0,
            len: 0,
        })
    }

    fn open_opendict(mdx_path: &PathBuf) -> Result<OpenedFile> {
        let dir = mdx_path
            .parent()
            .ok_or_else(|| ParserError::Validation("MDX 路径没有父目录".into()))?;
        let dict = opendict::mdict::MdictDictionary::open(dir)
            .map_err(|e| ParserError::Internal(format!("opendict 打开失败: {e}")))?;
        let header = dict.header();
        let sheet = parse_stylesheet(header.style_sheet.as_deref());
        let compact = header.compact;
        Ok(OpenedFile {
            backend: Backend::Opendict { dict },
            sheet,
            compact,
            base: 0,
            len: 0,
        })
    }

    /// 逐条词条迭代；sink 逐条回调
    fn for_each_entry(
        &mut self,
        files: &[PathBuf],
        rewrite_opts: Option<&ParseOpts>,
        mut sink: impl FnMut(ParsedEntry) -> Result<Flow>,
    ) -> Result<()> {
        let (mdx_paths, _mdd_paths) = Self::split_files(files);
        self.ensure_opened(&mdx_paths)?;
        let opened = self.opened.as_ref().expect("opened");
        // 逐文件、文件内按序号位置迭代（mdictlib 单槽块缓存对顺序访问友好）；
        // 样式表/Compact 用**该文件自己**的头部
        for file in &opened.files {
            for local in 0..file.len {
                let (word, text) = entry_text_at(file, local)?;
                // 先展开 `N` 样式标记、再改写资源引用：样式表的标签里本身可能含 src/href
                let mut definition = expand_style_markers(&text, &file.sheet, file.compact);
                if let Some(opts) = rewrite_opts {
                    if opts.rewrite_refs {
                        definition = res::rewrite_resource_refs(&definition, opts.dictionary_id);
                    }
                }
                match sink(ParsedEntry::new(word, definition))? {
                    Flow::Continue => {}
                    Flow::Stop => return Ok(()),
                }
            }
        }
        Ok(())
    }

    /// lite 物化：按源文件内序号读单条释义（只解压所在 record 块）。
    /// 处理管线与全量导入一致：样式展开 → 资源引用改写（rewrite_with=Some(词典id) 时）。
    pub fn definition_at(
        &mut self,
        files: &[PathBuf],
        ordinal: i64,
        rewrite_with: Option<i32>,
    ) -> Result<String> {
        let (mdx_paths, _) = Self::split_files(files);
        self.ensure_opened(&mdx_paths)?;
        let opened = self.opened.as_ref().expect("opened");
        let (file, local) = opened
            .locate(ordinal as u64)
            .ok_or_else(|| ParserError::Internal("词条序号越界".into()))?;
        let (word, text) = entry_text_at(file, local)?;
        let _ = word;
        let mut definition = expand_style_markers(&text, &file.sheet, file.compact);
        if let Some(dictionary_id) = rewrite_with {
            definition = res::rewrite_resource_refs(&definition, dictionary_id);
        }
        Ok(definition)
    }

    /// 轻量模式：只枚举词头与源文件内序号（纯 key 索引，不解压任何 record 块）
    fn for_each_headword(
        &mut self,
        files: &[PathBuf],
        mut sink: impl FnMut(Headword) -> Result<()>,
    ) -> Result<()> {
        let (mdx_paths, _) = Self::split_files(files);
        self.ensure_opened(&mdx_paths)?;
        let opened = self.opened.as_ref().expect("opened");
        for file in &opened.files {
            for local in 0..file.len {
                let word = match &file.backend {
                    Backend::Mdictlib { mdx, .. } => {
                        let Some(key) = mdx
                            .key_at(mdictlib::KeyOrdinal::from(local))
                            .map_err(|e| ParserError::Internal(format!("读词头失败: {e}")))?
                        else {
                            continue;
                        };
                        key.key().to_string()
                    }
                    Backend::Opendict { dict } => match dict.word_at(local as usize) {
                        Some(word) => word.to_string(),
                        None => continue,
                    },
                };
                sink(Headword {
                    word,
                    ordinal: (file.base + local) as i64,
                })?;
            }
        }
        Ok(())
    }
}

/// 在已打开的文件句柄上按**文件内**序号读一条 (word, 原始释义文本)
fn entry_text_at(file: &OpenedFile, local: u64) -> Result<(String, String)> {
    match &file.backend {
        Backend::Mdictlib { mdx, .. } => {
            let entry = mdx
                .entry_at(mdictlib::KeyOrdinal::from(local))
                .map_err(|e| ParserError::Internal(format!("读取词条失败: {e}")))?
                .ok_or_else(|| ParserError::Internal("词条序号越界".into()))?;
            Ok((entry.key().to_string(), entry.text().to_string()))
        }
        Backend::Opendict { dict } => Ok(dict
            .entry_at(local as usize)?
            .ok_or_else(|| ParserError::Internal("词条序号越界".into()))?),
    }
}

fn large_limits() -> mdictlib::Limits {
    // 默认 2M 词头上限不够（搜韵 826 万词头），用高配额预设
    mdictlib::Limits::large_dictionary()
}

fn map_mdictlib_err(path: &PathBuf) -> impl Fn(mdictlib::Error) -> ParserError + '_ {
    move |err: mdictlib::Error| match err {
        mdictlib::Error::Unsupported(feature) => ParserError::Unsupported(format!(
            "{}: {}",
            path.display(),
            feature
        )),
        other => ParserError::Internal(format!("{}: {}", path.display(), other)),
    }
}

impl super::DictionaryParser for MdictParser {
    fn parse(
        &mut self,
        files: &[PathBuf],
        opts: &ParseOpts,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()> {
        let (mdx_paths, _mdd_paths) = Self::split_files(files);
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }

        // resource_dir 为 None：不落盘 .mdd 资源，也不改写释义里的资源引用
        // （样式标记仍然照常展开 —— 那是文字排版，与「不导入发音/图片」无关）
        if let Some(resource_dir) = &opts.resource_dir {
            self.extract_mdd_resources(files, resource_dir, opts.overwrite_resources)?;
            res::copy_sibling_resources(resource_dir, files);
        }

        // 复用已打开的 MDX 迭代词条，2000 条一批（对齐 Python BATCH_SIZE）
        self.ensure_opened(&mdx_paths)?;
        let mut batch: Vec<ParsedEntry> = Vec::with_capacity(2000);
        self.for_each_entry(files, Some(opts), |entry| {
            batch.push(entry);
            if batch.len() >= 2000 {
                let out = std::mem::take(&mut batch);
                emit(out)?;
            }
            Ok(Flow::Continue)
        })?;
        if !batch.is_empty() {
            emit(batch)?;
        }
        Ok(())
    }

    fn sample(&mut self, files: &[PathBuf], limit: usize) -> Result<Vec<ParsedEntry>> {
        let (mdx_paths, _) = Self::split_files(files);
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }
        // 只迭代到够 limit 条「有信息量」的词条为止，不加载 .mdd、不写盘
        let mut sampled = Vec::new();
        let mut scanned = 0usize;
        let scan_cap = limit * SAMPLE_SCAN_FACTOR;
        self.ensure_opened(&mdx_paths)?;
        self.for_each_entry(files, None, |entry| {
            scanned += 1;
            if scanned > scan_cap {
                // 达扫描上限：返回已收集的，不再扫
                return Ok(Flow::Stop);
            }
            // 扫描版词典整条释义就是一个 <img>，索引项（纯数字词头）同理——跳过
            if !is_informative(&entry.word, Some(&entry.definition)) {
                return Ok(Flow::Continue);
            }
            sampled.push(entry);
            if sampled.len() >= limit {
                return Ok(Flow::Stop);
            }
            Ok(Flow::Continue)
        })?;
        Ok(sampled)
    }

    fn sample_headwords(&mut self, files: &[PathBuf], limit: usize) -> Result<Vec<String>> {
        let (mdx_paths, _) = Self::split_files(files);
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }
        self.ensure_opened(&mdx_paths)?;
        let opened = self.opened.as_ref().expect("opened");
        let total = opened.total() as usize;

        // 跨度按 2 倍目标取（词头里难免混着索引项），并且跑完整段再统一下采样
        // ——中途收满就停会退回「只取开头」
        let step = std::cmp::max(1, total / (limit * 2));
        let mut collected: Vec<String> = Vec::new();
        let mut index = 0usize;
        while index < total {
            let Some((file, local)) = opened.locate(index as u64) else {
                index += step;
                continue;
            };
            let word = match &file.backend {
                Backend::Mdictlib { mdx, .. } => {
                    let Some(key) = mdx
                        .key_at(mdictlib::KeyOrdinal::from(local))
                        .map_err(|e| ParserError::Internal(format!("读词头失败: {e}")))?
                    else {
                        index += step;
                        continue;
                    };
                    key.key().to_string()
                }
                Backend::Opendict { dict } => {
                    match dict.entry_at(local as usize)? {
                        Some((word, _)) => word,
                        None => {
                            index += step;
                            continue;
                        }
                    }
                }
            };
            if is_informative_headword(&word) {
                collected.push(word);
            }
            index += step;
        }
        Ok(spread_downsample(collected, limit))
    }

    fn parse_headwords(
        &mut self,
        files: &[PathBuf],
        emit: &mut dyn FnMut(Vec<Headword>) -> Result<()>,
    ) -> Result<()> {
        let (mdx_paths, _) = Self::split_files(files);
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }
        self.ensure_opened(&mdx_paths)?;
        let mut batch: Vec<Headword> = Vec::with_capacity(2000);
        self.for_each_headword(files, |headword| {
            batch.push(headword);
            if batch.len() >= 2000 {
                let out = std::mem::take(&mut batch);
                emit(out)?;
            }
            Ok(())
        })?;
        if !batch.is_empty() {
            emit(batch)?;
        }
        Ok(())
    }

    fn parse_parallel(
        &mut self,
        files: &[PathBuf],
        opts: &ParseOpts,
        workers: usize,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()> {
        let (mdx_paths, _) = Self::split_files(files);
        if mdx_paths.is_empty() {
            return Err(ParserError::Validation("MDict 词典缺少 .mdx 文件".into()));
        }
        // 资源解包/兄弟复制是一次性的，留在主线程串行做
        if let Some(resource_dir) = &opts.resource_dir {
            self.extract_mdd_resources(files, resource_dir, opts.overwrite_resources)?;
            res::copy_sibling_resources(resource_dir, files);
        }

        // 探路一次拿总条数（worker 各自再打开独立句柄）
        self.ensure_opened(&mdx_paths)?;
        let total = self.opened.as_ref().expect("opened").total() as usize;
        if total == 0 {
            return Ok(());
        }
        // 多 .mdx 组：worker 也要打开全部文件并复刻 base 偏移，才能按全局区间寻址
        let all_mdx: Vec<PathBuf> = mdx_paths.iter().map(|p| (*p).clone()).collect();
        // CPU 占用控制：worker 数封顶（导入任务本身经 bulk_write 串行排队，
        // 多词典同时转换也不会叠加并行度）；sync_channel(2) 提供批间背压
        let workers = workers.clamp(1, 16).min(total);
        let chunk = total.div_ceil(workers);
        let rewrite = opts.rewrite_refs;
        let dictionary_id = opts.dictionary_id;

        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<ParsedEntry>>(2);
        let mut first_error: Option<ParserError> = None;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for k in 0..workers {
                let lo = k * chunk;
                let hi = (lo + chunk).min(total);
                if lo >= hi {
                    break;
                }
                let tx = tx.clone();
                let paths = all_mdx.clone();
                handles.push(scope.spawn(move || -> Result<()> {
                    // 打开全部 .mdx 并复刻 base 偏移（与主线程 ensure_opened 同一算法）
                    let mut files = Vec::with_capacity(paths.len());
                    let mut base = 0u64;
                    for path in &paths {
                        let mut file = open_backend(path)?;
                        file.len = match &file.backend {
                            Backend::Mdictlib { mdx, .. } => mdx.len(),
                            Backend::Opendict { dict } => dict.entry_count() as u64,
                        };
                        file.base = base;
                        base += file.len;
                        files.push(file);
                    }
                    let mut batch: Vec<ParsedEntry> = Vec::with_capacity(2000);
                    for ordinal in lo as u64..hi as u64 {
                        let Some((file, local)) =
                            files.iter().find(|f| ordinal >= f.base && ordinal < f.base + f.len)
                                .map(|f| (f, ordinal - f.base))
                        else {
                            continue;
                        };
                        let (word, text) = entry_text_at(file, local)?;
                        let mut definition =
                            expand_style_markers(&text, &file.sheet, file.compact);
                        if rewrite {
                            definition = res::rewrite_resource_refs(&definition, dictionary_id);
                        }
                        batch.push(ParsedEntry::new(word, definition));
                        if batch.len() >= 2000 {
                            // 接收端已退出（emit 出错）→ 提前收工，不算错误
                            if tx.send(std::mem::take(&mut batch)).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    let _ = tx.send(batch);
                    Ok(())
                }));
            }
            drop(tx);
            // 主线程排空通道 → 单线程顺序回调 emit（DB 插入侧不并行）
            let mut emit_error: Option<ParserError> = None;
            while let Ok(batch) = rx.recv() {
                if let Err(e) = emit(batch) {
                    emit_error = Some(e);
                    break;
                }
            }
            // 先 drop 接收端再 join：阻塞中的 worker send 失败后自行退出
            drop(rx);
            first_error = emit_error;
            for handle in handles {
                let outcome = handle.join().unwrap_or_else(|_| {
                    Err(ParserError::Internal("解析线程崩溃".into()))
                });
                if let Err(e) = outcome {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        });
        if let Some(e) = first_error {
            return Err(e);
        }
        Ok(())
    }
}

impl MdictParser {
    /// 解包全部 .mdd 资源到 resource_dir（保留 mdd 内部相对层级）
    fn extract_mdd_resources(
        &mut self,
        files: &[PathBuf],
        resource_dir: &PathBuf,
        overwrite: bool,
    ) -> Result<()> {
        let (_, mdd_paths) = Self::split_files(files);
        if mdd_paths.is_empty() {
            return Ok(());
        }
        let (mdx_paths, _) = Self::split_files(files);
        let use_opendict = matches!(
            self.ensure_opened(&mdx_paths)?.files.first().map(|f| &f.backend),
            Some(Backend::Opendict { .. })
        );
        if use_opendict {
            // v3：fork 补丁的枚举接口
            let keys: Vec<String> = match &self
                .opened
                .as_ref()
                .expect("opened")
                .files
                .first()
                .map(|f| &f.backend)
            {
                Some(Backend::Opendict { dict }) => dict.mdd_resource_keys(),
                _ => unreachable!(),
            };
            for key in keys {
                let content = match &self
                    .opened
                    .as_ref()
                    .expect("opened")
                    .files
                    .first()
                    .map(|f| &f.backend)
                {
                    Some(Backend::Opendict { dict }) => dict.lookup_resource(&key),
                    _ => unreachable!(),
                };
                if let Some(content) = content {
                    res::write_resource(resource_dir, &key, &content, overwrite)?;
                }
            }
            return Ok(());
        }
        for mdd_path in mdd_paths {
            let mdd = MddFile::open_with_options(
                mdd_path,
                &mdictlib::OpenOptions::default().with_limits(large_limits()),
            )
            .map_err(|e| ParserError::Internal(format!("{}: {e}", mdd_path.display())))?;
            // keys() 借用 mdd，先收集再逐个 lookup
            let mut keys: Vec<String> = Vec::new();
            for key in mdd.keys() {
                let key = key
                    .map_err(|e| ParserError::Internal(format!("{}: {e}", mdd_path.display())))?;
                keys.push(key.key().to_string());
            }
            for key in keys {
                let Some(resource) = mdd
                    .lookup(&key)
                    .map_err(|e| ParserError::Internal(format!("{}: {e}", mdd_path.display())))?
                else {
                    continue;
                };
                res::write_resource(resource_dir, &key, resource.bytes(), overwrite)?;
            }
        }
        Ok(())
    }
}
