//! 运行期直接从 .mdd 按需读取资源 —— 替代导入期全量解包的磁盘优化。
//!
//! mdictlib 的 MddFile 支持单资源随机访问（只解压所在块），配合进程内：
//! - 句柄缓存（每部 .mdd 打开一次，key 索引常驻）
//! - 归一化键映射（mdd 键是 Windows 风格 `\dir\file.png` 且大小写不定，请求路径
//!   统一归一成小写正斜杠后映射回原始键；仅在精确查找未命中时才构建）
//! - 字节缓存（256MB 上限，按字节计重；词条整页图片这类热资源命中后零解压）
//!
//! 查找顺序与 /dict-res 路由一致：先精确（正/反斜杠、带/不带前导斜杠），
//! 再走归一化映射兜底（词典多在 Windows 打包，引用与键的大小写常不一致）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use mdictlib::MddFile;
use moka::sync::Cache as MokaCache;
use sea_orm::{ConnectionTrait, Statement};

use crate::AppState;

/// 句柄缓存容量（同时服务的 .mdd 个数上限；打开 = 解析 key 索引，毫秒级但没必要重复）
const MDD_HANDLE_CACHE: usize = 8;
/// 资源字节缓存上限（按字节数计重）
const RESOURCE_BYTES_CACHE: usize = 256 * 1024 * 1024;

pub struct MddResources {
    handles: MokaCache<PathBuf, Arc<MddHandle>>,
    bytes: MokaCache<String, Arc<Vec<u8>>>,
}

struct MddHandle {
    file: MddFile,
    /// 归一化键（小写正斜杠）→ 原始键；首次大小写兜底时构建
    lower_map: OnceLock<HashMap<String, String>>,
}

impl MddResources {
    pub fn new() -> Self {
        Self {
            handles: MokaCache::builder()
                .max_capacity(MDD_HANDLE_CACHE as u64)
                .build(),
            bytes: MokaCache::builder()
                .max_capacity(RESOURCE_BYTES_CACHE as u64)
                .weigher(|_k, v: &Arc<Vec<u8>>| v.len() as u32)
                .build(),
        }
    }

    pub fn invalidate_dictionary(&self, dictionary_id: i32) {
        // moka 没有「按前缀失效」；字典删除不常见，直接全清（重建成本 = 重新打开句柄）
        let _ = dictionary_id;
        self.handles.invalidate_all();
        self.bytes.invalidate_all();
    }
}

fn normalize_key(key: &str) -> String {
    key.replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase()
}

impl MddHandle {
    fn open(path: &PathBuf) -> Result<Self, mdictlib::Error> {
        let options = mdictlib::OpenOptions::default()
            .with_limits(mdictlib::Limits::large_dictionary());
        let file = MddFile::open_with_options(path, &options)?;
        Ok(Self {
            file,
            lower_map: OnceLock::new(),
        })
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
        // 大小写兜底：构建归一化映射
        let map = self.lower_map.get_or_init(|| {
            let mut map = HashMap::new();
            for key in self.file.keys().flatten() {
                let owned = key.key().to_string();
                map.entry(normalize_key(&owned)).or_insert(owned);
            }
            map
        });
        let exact = map.get(&normalize_key(rel_path))?;
        self.file
            .lookup(exact)
            .ok()
            .flatten()
            .map(|r| r.bytes().to_vec())
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
    for extension in [".css", ".js"] {
        let name = format!("{stem}{extension}");
        if assets.iter().any(|(n, _)| *n == name) {
            continue;
        }
        if resource_exists(state, dictionary_id, &name).await {
            assets.push((
                name.clone(),
                format!(
                    "/dict-res/{dictionary_id}/res/{}",
                    dict_parser::resources::quote_path_pub(&name)
                ),
            ));
        }
    }
    assets
}
