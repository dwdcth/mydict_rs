use sea_orm_migration::prelude::*;

/// 词典组（GoldenDict 式）：每个 Web 用户可自建若干命名组，
/// 查询时选定一组 → 只在该组的词典范围内查（组内顺序即结果优先级顺序）。
///
/// - dictionary_groups：组（user 专属，组名同用户内唯一）
/// - dictionary_group_items：组的词典成员（保序；dictionary 删除时级联清掉）
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute_raw(sea_orm::Statement::from_string(
                backend,
                // SQLite 的 REFERENCES 列必须可为 NULL 才允许 ALTER DROP，这里两列均 NOT NULL
                //（组员表随组级联删除，不需要 DROP）
                r#"CREATE TABLE dictionary_groups (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    name TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    UNIQUE (user_id, name)
                );
                CREATE TABLE dictionary_group_items (
                    group_id INTEGER NOT NULL REFERENCES dictionary_groups(id) ON DELETE CASCADE,
                    dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (group_id, dictionary_id)
                );
                CREATE INDEX ix_dict_group_items_group ON dictionary_group_items (group_id, position);"#
                    .to_string(),
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_raw(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "DROP TABLE dictionary_group_items; DROP TABLE dictionary_groups;".to_string(),
            ))
            .await?;
        Ok(())
    }
}
