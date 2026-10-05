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
    backend: Backend,
    sheet: HashMap<String, (String, String)>,
    compact: bool,
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
        let mdx_path = mdx_paths
            .first()
            .ok_or_else(|| ParserError::Validation("MDict 词典缺少 .mdx 文件".into()))?;

        let opened = match probe_family(mdx_path) {
            Some(MdictFamily::Opendict) => self.open_opendict(mdx_path)?,
            Some(MdictFamily::Mdictlib) | None => match self.open_mdictlib(mdx_path) {
                Ok(opened) => opened,
                Err(ParserError::Unsupported(reason)) => {
                    // probe 失败的文件可能是 mdictlib 不认识的版本（如 v3 加密头）——回退
                    tracing::info!(reason = %reason, "mdictlib 不支持，回退 opendict 后端");
                    self.open_opendict(mdx_path)?
                }
                Err(other) => return Err(other),
            },
        };
        self.opened = Some(opened);
        Ok(self.opened.as_mut().expect("just set"))
    }

    fn open_mdictlib(&self, mdx_path: &PathBuf) -> Result<Opened> {
        let options = mdictlib::OpenOptions::default().with_limits(large_limits());
        let mdx = MdxFile::open_with_options(mdx_path, &options)
            .map_err(map_mdictlib_err(mdx_path))?;
        let header = mdx.header();
        let sheet = parse_stylesheet(header.attribute("StyleSheet"));
        let compact = is_compact_value(header.attribute("Compact"));
        Ok(Opened {
            backend: Backend::Mdictlib { mdx },
            sheet,
            compact,
        })
    }

    fn open_opendict(&self, mdx_path: &PathBuf) -> Result<Opened> {
        let dir = mdx_path
            .parent()
            .ok_or_else(|| ParserError::Validation("MDX 路径没有父目录".into()))?;
        let dict = opendict::mdict::MdictDictionary::open(dir)
            .map_err(|e| ParserError::Internal(format!("opendict 打开失败: {e}")))?;
        let header = dict.header();
        let sheet = parse_stylesheet(header.style_sheet.as_deref());
        let compact = header.compact;
        Ok(Opened {
            backend: Backend::Opendict { dict },
            sheet,
            compact,
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
        let (sheet, compact) = {
            let opened = self.ensure_opened(&mdx_paths)?;
            (opened.sheet.clone(), opened.compact)
        };
        // 后端按序号位置访问统一迭代（mdictlib 单槽块缓存对顺序访问友好；
        // opendict 的 keywords 物化 + record_at 位置读）
        let total = match &self.opened.as_ref().expect("opened").backend {
            Backend::Mdictlib { mdx, .. } => mdx.len(),
            Backend::Opendict { dict } => dict.entry_count() as u64,
        };
        for ordinal in 0..total {
            let (word, text) = match &self.opened.as_ref().expect("opened").backend {
                Backend::Mdictlib { mdx, .. } => {
                    let entry = mdx
                        .entry_at(mdictlib::KeyOrdinal::from(ordinal))
                        .map_err(|e| ParserError::Internal(format!("读取词条失败: {e}")))?
                        .ok_or_else(|| ParserError::Internal("词条序号越界".into()))?;
                    (entry.key().to_string(), entry.text().to_string())
                }
                Backend::Opendict { dict } => dict
                    .entry_at(ordinal as usize)?
                    .ok_or_else(|| ParserError::Internal("词条序号越界".into()))?,
            };
            // 先展开 `N` 样式标记、再改写资源引用：样式表的标签里本身可能含 src/href
            let mut definition = expand_style_markers(&text, &sheet, compact);
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
        Ok(())
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
        let opened = self.ensure_opened(&mdx_paths)?;
        let total = match &opened.backend {
            Backend::Mdictlib { mdx, .. } => mdx.len(),
            Backend::Opendict { dict } => dict.entry_count() as u64,
        } as usize;

        // 跨度按 2 倍目标取（词头里难免混着索引项），并且跑完整段再统一下采样
        // ——中途收满就停会退回「只取开头」
        let step = std::cmp::max(1, total / (limit * 2));
        let mut collected: Vec<String> = Vec::new();
        let mut index = 0usize;
        while index < total {
            let word = match &self.opened.as_ref().expect("opened").backend {
                Backend::Mdictlib { mdx, .. } => {
                    let Some(key) = mdx
                        .key_at(mdictlib::KeyOrdinal::from(index as u64))
                        .map_err(|e| ParserError::Internal(format!("读词头失败: {e}")))?
                    else {
                        index += step;
                        continue;
                    };
                    key.key().to_string()
                }
                Backend::Opendict { dict } => {
                    match dict.entry_at(index)? {
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
            self.ensure_opened(&mdx_paths)?.backend,
            Backend::Opendict { .. }
        );
        if use_opendict {
            // v3：fork 补丁的枚举接口
            let keys: Vec<String> = match &self.opened.as_ref().expect("opened").backend {
                Backend::Opendict { dict } => dict.mdd_resource_keys(),
                _ => unreachable!(),
            };
            for key in keys {
                let content = match &self.opened.as_ref().expect("opened").backend {
                    Backend::Opendict { dict } => dict.lookup_resource(&key),
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
