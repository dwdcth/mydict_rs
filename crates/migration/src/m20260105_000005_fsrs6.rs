use sea_orm_migration::prelude::*;

/// FSRS 调度器升级到官方 fsrs crate（FSRS-6，21 参数）：
/// - flashcards.stability_fast：FSRS-7 的快稳定性轨迹（FSRS-6 下恒等于 stability；
///   为将来兼容 FSRS-7 权重预留，旧数据按 stability 回填）
/// - flashcards.last_rating：最近一次评分（0=未评过；驱动「重学中」状态显示）
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_raw(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "ALTER TABLE flashcards ADD COLUMN stability_fast REAL NOT NULL DEFAULT 0;
                 ALTER TABLE flashcards ADD COLUMN last_rating INTEGER NOT NULL DEFAULT 0;
                 UPDATE flashcards SET stability_fast = stability;"
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
                "ALTER TABLE flashcards DROP COLUMN stability_fast;
                 ALTER TABLE flashcards DROP COLUMN last_rating;"
                    .to_string(),
            ))
            .await?;
        Ok(())
    }
}
