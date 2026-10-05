//! 词条可见域与查询条件 —— 移植自 `app/services/entry_scope.py`。
//!
//! 所有读 dict_entries 的查询都必须限定「当前代」：JOIN dictionaries 且
//! generation = active_generation。「代」机制见 migration 注释。
//!
//! 这里的三个 SQL 片段是性能行为的锚点（Python 版有测试逐条锁定）：
//! - 前缀匹配用区间比较而不是 LIKE（LIKE 默认大小写不敏感且 %/_ 是通配符）
//! - `dictionary_id + 0 = ?` 故意禁用索引、强制走主键区间（分批清理用）

/// JOIN 片段：dict_entries（别名 e）限定当前代，字典表（别名 d）
pub const CURRENT_GENERATION_JOIN: &str =
    "JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation";

/// 前缀区间条件（参数：prefix）。上界 = prefix + U+10FFFF（BINARY 排序下即前缀上界）。
/// 用参数化而非字符串拼接，调用方以 `$n` 占位符引用。
pub fn word_lower_prefix_condition(param_index: usize) -> String {
    format!(
        "e.word_lower >= ${param_index} AND e.word_lower < ${param_index} || char(1114111)"
    )
}

/// SQLite：char(1114111) 是 U+10FFFF 的 char() 函数写法；但 SQLite 的 char() 用 UTF-16
/// 码元解释参数，BMP 外字符会得到代理对——Python 版是应用层拼 `prefix + "\U0010ffff"`。
/// 为行为一致，这里也由调用方在应用层拼好上界，本函数保留作对照说明。
pub fn word_lower_prefix_bounds(prefix: &str) -> (String, String) {
    let upper = format!("{prefix}\u{10FFFF}");
    (prefix.to_string(), upper)
}

/// 分批清理的主键区间条件（SQLite 专属 hack：+0 禁用 dictionary_id 索引，
/// 强制查询沿主键顺序扫区间，实测从秒级降到毫秒级）
pub fn in_dictionary_id_window_sql(dictionary_id: i32) -> String {
    format!("dictionary_id + 0 = {dictionary_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_bounds_attach_max_codepoint() {
        let (lower, upper) = word_lower_prefix_bounds("app");
        assert_eq!(lower, "app");
        assert_eq!(upper, "app\u{10FFFF}");
        // 区间 [lower, upper) 恰好覆盖全部 app 前缀词、不含 apq
        assert!("apple" >= lower.as_str() && "apple" < upper.as_str());
        assert!("app" >= lower.as_str() && "app" < upper.as_str());
        assert!("apq" < lower.as_str() || "apq" >= upper.as_str());
    }
}
