//! 词典格式解析库（MDict v1/v2/v3、StarDict、ECDICT）—— 统一 `DictionaryParser` 接口，
//! 对应 Python 版 `app/parsers/base.py` 的三方法契约（parse / sample / sample_headwords）。

pub mod ecdict;
pub mod entry;
pub mod mdict;
pub mod probe;
pub mod resources;
pub mod stardict;

pub use entry::{
    is_informative, is_informative_headword, spread_downsample, strip_markup, ParsedEntry,
    SAMPLE_SCAN_FACTOR,
};

use std::path::PathBuf;

/// 解析错误统一类型：
/// - `Validation` 对应 Python 版的 `ValueError`（上层转 422 ValidationAppError）
/// - `Unsupported` 对应 mdictlib 的 `Error::Unsupported`（如 v1 加密文件）
#[derive(thiserror::Error, Debug)]
pub enum ParserError {
    #[error("{0}")]
    Validation(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, ParserError>;

impl From<opendict::Error> for ParserError {
    fn from(err: opendict::Error) -> Self {
        ParserError::Internal(format!("opendict: {err}"))
    }
}

/// parse() 的选项（对齐 Python parse 的关键字参数）
#[derive(Debug, Clone)]
pub struct ParseOpts {
    pub dictionary_id: i32,
    /// None = 不落盘 .mdd 资源、不改写释义里的资源引用（skip_resources / 无 res 目录）
    pub resource_dir: Option<PathBuf>,
    /// false 时已存在的资源文件保持不动，只补缺失的（重新解析用）
    pub overwrite_resources: bool,
    /// 是否把释义里的资源引用改写为 /dict-res 绝对路径（resource_dir 为 None 时必为 false）
    pub rewrite_refs: bool,
}

impl ParseOpts {
    pub fn new(dictionary_id: i32) -> Self {
        Self {
            dictionary_id,
            resource_dir: None,
            overwrite_resources: true,
            rewrite_refs: false,
        }
    }

    /// 只改写释义里的资源引用（/dict-res 绝对路径），不解包 .mdd 到磁盘。
    /// 运行期由 /dict-res 路由直接从 .mdd 按需读取资源。
    pub fn with_rewrite(mut self) -> Self {
        self.rewrite_refs = true;
        self
    }

    /// 改写引用并解包 .mdd 到磁盘（磁盘换性能的可选项）
    pub fn with_resources(mut self, dir: PathBuf, overwrite: bool) -> Self {
        self.rewrite_refs = true;
        self.resource_dir = Some(dir);
        self.overwrite_resources = overwrite;
        self
    }
}

/// 轻量挂载模式的词头条目：词头 + 源文件内定位
/// （mdx 词条序号 / stardict idx 下标；stardict 别名行的 ordinal 指向目标条目）
#[derive(Debug, Clone)]
pub struct Headword {
    pub word: String,
    pub ordinal: i64,
}

/// 三种词典格式解析器的统一接口 —— 对齐 Python `DictionaryParser`。
pub trait DictionaryParser: Send {
    /// 流式解析，批量回调交付（emit 每次最多 2000 条）。
    /// resource_dir=Some 时负责 MDD 资源落盘 + 兄弟资源拷贝 + definition 引用改写。
    fn parse(
        &mut self,
        files: &[PathBuf],
        opts: &ParseOpts,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()>;

    /// 只读采样至多 limit 条**有信息量**的词条，供语言识别使用。
    /// 绝不写盘：不落 .mdd 资源、不改写引用、不创建任何目录；读取量有上界。
    fn sample(&mut self, files: &[PathBuf], limit: usize) -> Result<Vec<ParsedEntry>>;

    /// 跨**整部**词典均匀取至多 limit 个词头（先跨完整段收集、再统一下采样）。
    fn sample_headwords(&mut self, files: &[PathBuf], limit: usize) -> Result<Vec<String>>;

    /// 并行全量解析（lite→full 后台转换等 CPU 密集场景用）。
    /// workers 个线程各开独立句柄、迭代不相交的序号区间；emit 仍单线程顺序回调。
    /// 默认实现退回串行 parse（ECDICT/StarDict 走这条）。
    fn parse_parallel(
        &mut self,
        files: &[PathBuf],
        opts: &ParseOpts,
        workers: usize,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()> {
        let _ = workers;
        self.parse(files, opts, emit)
    }

    /// 轻量模式：只枚举词头与源文件定位（不读释义块）。批量回调交付。
    /// 默认不支持（ECDICT 是 CSV，没有随机访问的意义）。
    fn parse_headwords(
        &mut self,
        _files: &[PathBuf],
        _emit: &mut dyn FnMut(Vec<Headword>) -> Result<()>,
    ) -> Result<()> {
        Err(ParserError::Validation(
            "该格式不支持轻量导入（ECDICT 请使用全量模式）".into(),
        ))
    }
}

/// 按格式创建解析器
pub fn parser_by_format(format: &str) -> Result<Box<dyn DictionaryParser>> {
    match format {
        "mdict" => Ok(Box::new(mdict::MdictParser::new())),
        "stardict" => Ok(Box::new(stardict::StarDictParser::new())),
        "ecdict" => Ok(Box::new(ecdict::EcdictParser)),
        other => Err(ParserError::Validation(format!(
            "未知的词典格式: {other}"
        ))),
    }
}
