//! 运行期直接从 .mdd 按需读取资源 —— 替代导入期全量解包的磁盘优化。
//!
//! mdictlib 的 MddFile 支持单资源随机访问（只解压所在块），配合进程内：
//! - 句柄缓存（**按字节预算** LRU：打开时强制构建词头索引并读取
//!   mdictlib `memory_usage()` 实测占用计重；GoldenDict 靠分组限量，服务端用预算 + 空闲超时）
//! - 空闲回收（`time_to_idle`：DICT_IDLE_UNLOAD_SECS 未被查询的 .mdd 关闭句柄释放内存，
//!   下次查询重新打开——索引构建本来就在首个请求时付过一次）
//! - 归一化键映射（mdd 键是 Windows 风格 `\dir\file.png` 且大小写不定，请求路径
//!   统一归一成小写正斜杠后映射回**物理序号**（KeyOrdinal 直读，不再持有原始键串副本）；
//!   仅在精确查找未命中时才构建）
//! - 字节缓存（256MB 上限，按字节计重 + 同样空闲回收；词条整页图片这类热资源命中后零解压）
//!
//! 查找顺序与 /dict-res 路由一致：先精确（正/反斜杠、带/不带前导斜杠），
//! 再走归一化映射兜底（词典多在 Windows 打包，引用与键的大小写常不一致）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use mdictlib::{KeyOrdinal, MddFile};
use moka::sync::Cache as MokaCache;
use sea_orm::{ConnectionTrait, Statement};

use crate::AppState;

/// 资源字节缓存上限（按字节数计重）
const RESOURCE_BYTES_CACHE: usize = 256 * 1024 * 1024;
/// 单个句柄计重下限（打开本身有文件描述符/头信息等常驻）
const HANDLE_WEIGHT_FLOOR: u32 = 64 * 1024;

pub struct MddResources {
    handles: MokaCache<PathBuf, Arc<MddHandle>>,
    bytes: MokaCache<String, Arc<Vec<u8>>>,
}

struct MddHandle {
    file: MddFile,
    /// 归一化键索引（小写正斜杠 → 物理序号）与 basename 索引（小写文件名 → 物理序号），
    /// 首次大小写/basename 兜底时一并构建
    key_maps: OnceLock<KeyMaps>,
    /// 常驻内存估算（打开时 = mdictlib memory_usage 实测；KeyMaps 构建后追加），
    /// 供句柄缓存按字节预算淘汰
    weight: AtomicU32,
}

struct KeyMaps {
    lower: HashMap<String, KeyOrdinal>,
    basename: HashMap<String, KeyOrdinal>,
}

/// KeyMaps 两张 HashMap 的每条目粗估（键 String + 哈希桶 + KeyOrdinal）
const KEYMAP_BYTES_PER_ENTRY: u32 = 96;

impl MddResources {
    /// `handle_budget`：句柄缓存字节预算；`idle`：空闲回收时长（句柄与字节缓存同参）
    pub fn new(handle_budget: usize, idle: Duration) -> Self {
        Self {
            handles: MokaCache::builder()
                .max_capacity(handle_budget as u64)
                .weigher(|_k, v: &Arc<MddHandle>| v.weight.load(Ordering::Relaxed))
                .time_to_idle(idle)
                .build(),
            bytes: MokaCache::builder()
                .max_capacity(RESOURCE_BYTES_CACHE as u64)
                .weigher(|_k, v: &Arc<Vec<u8>>| v.len() as u32)
                .time_to_idle(idle)
                .build(),
        }
    }

    pub fn invalidate_dictionary(&self, dictionary_id: i32) {
        // moka 没有「按前缀失效」；字典删除不常见，直接全清（重建成本 = 重新打开句柄）
        let _ = dictionary_id;
        self.handles.invalidate_all();
        self.bytes.invalidate_all();
    }

    /// 触发 moka 维护任务：真正释放空闲过期条目占的内存（空闲期没有读写，
    /// 维护不会自动跑；由调度器周期调用）
    pub fn run_maintenance(&self) {
        self.handles.run_pending_tasks();
        self.bytes.run_pending_tasks();
    }

    /// 常驻内存估算合计（诊断/日志用，粗略：条目权重之和）。
    /// 先跑一次维护：insert 后未处理的条目对 iter() 不可见，直接读会少计
    pub fn resident_bytes(&self) -> u64 {
        self.handles.run_pending_tasks();
        self.bytes.run_pending_tasks();
        self.handles
            .iter()
            .map(|(_, v)| v.weight.load(Ordering::Relaxed) as u64)
            .sum::<u64>()
            + self
                .bytes
                .iter()
                .map(|(_, v)| v.len() as u64)
                .sum::<u64>()
    }
}

fn normalize_key(key: &str) -> String {
    key.replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase()
}

/// 把 usize 权重压进 u32（超过 u32::MAX 按 u32::MAX 计——预算语义下已足够）
fn clamp_weight(bytes: usize) -> u32 {
    bytes.min(u32::MAX as usize) as u32
}

impl MddHandle {
    fn open(path: &PathBuf) -> Result<Self, mdictlib::Error> {
        let options = mdictlib::OpenOptions::default()
            .with_limits(mdictlib::Limits::large_dictionary());
        let file = MddFile::open_with_options(path, &options)?;
        let handle = Self {
            file,
            key_maps: OnceLock::new(),
            weight: AtomicU32::new(HANDLE_WEIGHT_FLOOR),
        };
        // 词头索引（locator）是惰性构建的——主动触发一次（必然 miss 的探测键），
        // 让 memory_usage() 从一开始就反映真实常驻量，句柄缓存计重才准确。
        // 这次构建本来也会发生在首个真实查找上，这里只是提前到打开时（都在阻塞线程）。
        let _ = handle.file.locate("\u{0}mydict-probe");
        if let Ok(usage) = handle.file.memory_usage() {
            handle
                .weight
                .store(HANDLE_WEIGHT_FLOOR.max(clamp_weight(usage.current_bytes())), Ordering::Relaxed);
        }
        Ok(handle)
    }

    fn lookup(&self, rel_path: &str) -> Option<Vec<u8>> {
        // 精确尝试：mdd 键的常见形态 `\dir\file`、`\dir/file`、`dir\file`
        let bs = rel_path.replace('/', "\\");
        for candidate in [
            format!("\\{}", bs),
            format!("\\{rel_path}"),
            bs.clone(),
            rel_path.to_string(),
        ] {
            if let Ok(Some(resource)) = self.file.lookup(&candidate) {
                return Some(resource.bytes().to_vec());
            }
        }
        // 大小写兜底（归一化映射）
        let maps = self.maps();
        let normalized = normalize_key(rel_path);
        if let Some(ordinal) = maps.lower.get(&normalized) {
            if let Ok(Some(resource)) = self.file.resource_at(*ordinal) {
                return Some(resource.bytes().to_vec());
            }
        }
        // basename 兜底：词典内引用常带与 mdd 不同的目录前缀，只要文件名能对上
        let basename = normalized.rsplit('/').next().unwrap_or(&normalized).to_string();
        if let Some(ordinal) = maps.basename.get(&basename) {
            if let Ok(Some(resource)) = self.file.resource_at(*ordinal) {
                return Some(resource.bytes().to_vec());
            }
        }
        // 后缀包含兜底：引用路径是 mdd 键的后缀（键多出数字 ID 前缀目录等）
        let suffix = format!("/{normalized}");
        if let Some((_, ordinal)) = maps.lower.iter().find(|(k, _)| k.ends_with(&suffix)) {
            if let Ok(Some(resource)) = self.file.resource_at(*ordinal) {
                return Some(resource.bytes().to_vec());
            }
        }
        None
    }

    /// 大小写归一化与 basename 双索引（一次遍历建好，OnceLock 保证只建一次）。
    /// 存物理序号而非原始键串：兜底读取走 resource_at(ordinal)，不重复持有键内存
    fn maps(&self) -> &KeyMaps {
        self.key_maps.get_or_init(|| {
            let mut lower = HashMap::new();
            let mut basename: HashMap<String, KeyOrdinal> = HashMap::new();
            for (index, key) in self.file.keys().enumerate() {
                let Ok(key) = key else { continue };
                let ordinal = KeyOrdinal::new(index as u64);
                let norm = normalize_key(key.key());
                if let Some(name) = norm.rsplit('/').next() {
                    basename
                        .entry(name.to_string())
                        .or_insert(ordinal);
                }
                lower.entry(norm).or_insert(ordinal);
            }
            let built = KeyMaps { lower, basename };
            // 追加两张索引的估算重量，让缓存预算把这块也算上
            let entries = (built.lower.len() + built.basename.len()) as u32;
            let extra = KEYMAP_BYTES_PER_ENTRY.saturating_mul(entries);
            self.weight.fetch_add(extra, Ordering::Relaxed);
            built
        })
    }
}

/// 某词典的全部 .mdd 源文件（按导入时的 position 保序；含分卷 .1.mdd 等）
async fn mdd_paths_for(state: &AppState, dictionary_id: i32) -> Vec<PathBuf> {
    let Ok(rows) = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT s.path FROM dictionary_sources s \
             JOIN dictionaries d ON d.id = s.dictionary_id \
             WHERE s.dictionary_id = $1 AND LOWER(s.path) LIKE '%.mdd' \
             ORDER BY s.position",
            [dictionary_id.into()],
        ))
        .await
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| r.try_get::<String>("", "path").ok())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .collect()
}

/// 直接从词典的 .mdd 里按需读取一条资源；带 256MB 进程内字节缓存
pub async fn lookup_resource(
    state: &AppState,
    dictionary_id: i32,
    rel_path: &str,
) -> Option<Arc<Vec<u8>>> {
    let normalized = rel_path.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    if normalized.is_empty() {
        return None;
    }
    let cache_key = format!("{dictionary_id}/{normalized}");
    if let Some(cached) = state.mdd_resources.bytes.get(&cache_key) {
        return Some(cached);
    }
    // mdd 打开与查找是阻塞 IO（mmap + 单块解压），放阻塞线程
    let paths = mdd_paths_for(state, dictionary_id).await;
    let handles = state.mdd_resources.handles.clone();
    let rel = normalized.to_string();
    let found = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        for path in &paths {
            let handle = match handles.get(path) {
                Some(handle) => handle,
                None => match MddHandle::open(path) {
                    Ok(handle) => {
                        let handle = Arc::new(handle);
                        handles.insert(path.clone(), handle.clone());
                        handle
                    }
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "打开 .mdd 失败");
                        continue;
                    }
                },
            };
            if let Some(bytes) = handle.lookup(&rel) {
                return Some(bytes);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()?;
    let arc = Arc::new(found);
    state.mdd_resources.bytes.insert(cache_key, arc.clone());
    Some(arc)
}

/// 资源是否存在（uss 喇叭清理等存在性检查用）
pub async fn resource_exists(state: &AppState, dictionary_id: i32, rel_path: &str) -> bool {
    lookup_resource(state, dictionary_id, rel_path).await.is_some()
}

/// 同名 .css/.js 附属资源：磁盘 res/ 里有的 + 只存在于 .mdd 里的一并注入词条文档
pub async fn same_name_assets_with_mdd(
    state: &AppState,
    dictionary_id: i32,
    res_dir: &std::path::Path,
    source_file: &str,
) -> Vec<(String, String)> {
    let mut assets = dict_parser::resources::same_name_assets(res_dir, dictionary_id, source_file);
    if assets.len() >= 2 {
        return assets; // css/js 都在
    }
    let stem = std::path::Path::new(source_file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if stem.is_empty() {
        return assets;
    }
    // 候选名链：同名 css/js 之外，还有词典内置路由名（__style.css 等）与
    // jquery 伴生变体（OALD 类词典的交互按钮依赖 {名}-jquery.js / jquery.js）。
    // css/js 各注入**首个命中**的一个（与既有 same_name_assets 的语义一致）
    let css_candidates = [format!("{stem}.css"), "__style.css".to_string()];
    let js_candidates = [
        format!("{stem}.js"),
        format!("{stem}-jquery.js"),
        "jquery.js".to_string(),
        "__script.js".to_string(),
        "__jquery.js".to_string(),
    ];
    let groups: [(Vec<String>, &str); 2] = [
        (css_candidates.to_vec(), ".css"),
        (js_candidates.to_vec(), ".js"),
    ];
    for (candidates, extension) in groups {
        if assets.iter().any(|(n, _)| n.ends_with(extension)) {
            continue;
        }
        for name in candidates {
            if resource_exists(state, dictionary_id, &name).await {
                assets.push((
                    name.clone(),
                    format!(
                        "/dict-res/{dictionary_id}/res/{}",
                        dict_parser::resources::quote_path_pub(&name)
                    ),
                ));
                break;
            }
        }
    }
    assets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn corpus_mdd() -> Option<PathBuf> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/mdx_v2_basic/basic.mdd");
        path.is_file().then_some(path)
    }

    /// 真实 .mdd 上的句柄行为：精确命中、大小写/分隔符兜底、basename 兜底、计重 > 0
    #[test]
    fn mdd_handle_lookup_and_weight() {
        let Some(path) = corpus_mdd() else {
            eprintln!("skipping: testdata/mdx_v2_basic/basic.mdd 不存在（先跑 scripts/gen_corpus.py）");
            return;
        };
        let handle = MddHandle::open(&path).expect("打开 basic.mdd");
        // 精确命中（正斜杠请求路径）
        assert!(handle.lookup("style.css").unwrap().starts_with(b"body {"));
        // 分隔符 + 大小写兜底（mdd 键是 \img\logo.png）
        assert!(handle.lookup("IMG/LOGO.PNG").unwrap().starts_with(b"\x89PNG"));
        // basename 兜底（引用带不同目录前缀）
        assert!(handle.lookup("whatever/dir/logo.png").unwrap().starts_with(b"\x89PNG"));
        // 未命中
        assert!(handle.lookup("nope.bin").is_none());
        // 打开即计重（locator 已在 open 里构建）
        assert!(
            handle.weight.load(Ordering::Relaxed) > 0,
            "weight 应反映 memory_usage"
        );
    }

    /// KeyMaps 构建后权重追加、且二次 maps() 不再重复计重
    #[test]
    fn mdd_handle_keymap_weight_accumulates_once() {
        let Some(path) = corpus_mdd() else {
            eprintln!("skipping: testdata 缺失");
            return;
        };
        let handle = MddHandle::open(&path).expect("打开 basic.mdd");
        let before = handle.weight.load(Ordering::Relaxed);
        let maps = handle.maps();
        let entries = (maps.lower.len() + maps.basename.len()) as u32;
        let after = handle.weight.load(Ordering::Relaxed);
        assert_eq!(
            after - before,
            KEYMAP_BYTES_PER_ENTRY * entries,
            "KeyMaps 计重应恰好追加一次"
        );
        let _ = handle.maps();
        assert_eq!(handle.weight.load(Ordering::Relaxed), after, "不重复计重");
    }
}
