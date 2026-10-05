use sea_orm_migration::prelude::*;

/// 轻量挂载模式（lite）：词头索引入库，释义运行期从源文件按需读取。
///
/// - dict_entries.source_ordinal：远程行在源文件内的定位
///   （mdx 的词条序号 / stardict 的 idx 下标）；NULL = 全量行（definition 已落库）。
///   远程行 definition 存空串（列是 NOT NULL，避免重建表）。
/// - dictionaries.entry_mode：'full' | 'lite'，重解析按原模式重灌。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE dict_entries ADD COLUMN source_ordinal INTEGER;
                 ALTER TABLE dictionaries ADD COLUMN entry_mode TEXT NOT NULL DEFAULT 'full';
                 ALTER TABLE dictionaries ADD COLUMN skip_resource_rewrite BOOLEAN NOT NULL DEFAULT 0;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite 不支持 DROP COLUMN（旧版本）；重建表代价大，down 仅在可 DROP 时执行
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE dict_entries DROP COLUMN source_ordinal;
                 ALTER TABLE dictionaries DROP COLUMN entry_mode;
                 ALTER TABLE dictionaries DROP COLUMN skip_resource_rewrite;",
            )
            .await?;
        Ok(())
    }
}
