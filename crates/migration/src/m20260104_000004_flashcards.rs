use sea_orm_migration::prelude::*;

/// 间隔重复复习（FSRS）：闪卡 = 生词本条目 + 调度信息。
///
/// - flashcards：挂在 vocab_items 上的调度状态（rs-fsrs 的 Card 字段一一对应；
///   PK 即 vocab_item_id，删生词本条目级联删卡与日志）
/// - flashcard_reviews：复习日志（评分/状态迁移/计划间隔——将来做统计或权重训练的数据源）
/// - users.fsrs_retention / fsrs_weights：每用户的 FSRS 参数（NULL = 默认 0.9 / 默认 19 权重）
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_raw(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                r#"CREATE TABLE flashcards (
                    vocab_item_id INTEGER PRIMARY KEY REFERENCES vocab_items(id) ON DELETE CASCADE,
                    state INTEGER NOT NULL DEFAULT 0,
                    due INTEGER NOT NULL,
                    stability REAL NOT NULL DEFAULT 0,
                    difficulty REAL NOT NULL DEFAULT 0,
                    elapsed_days INTEGER NOT NULL DEFAULT 0,
                    scheduled_days INTEGER NOT NULL DEFAULT 0,
                    reps INTEGER NOT NULL DEFAULT 0,
                    lapses INTEGER NOT NULL DEFAULT 0,
                    last_review INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX ix_flashcards_due ON flashcards (due);
                CREATE TABLE flashcard_reviews (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    flashcard_id INTEGER NOT NULL REFERENCES flashcards(vocab_item_id) ON DELETE CASCADE,
                    rating INTEGER NOT NULL,
                    state INTEGER NOT NULL,
                    elapsed_days INTEGER NOT NULL,
                    scheduled_days INTEGER NOT NULL,
                    reviewed_at INTEGER NOT NULL
                );
                CREATE INDEX ix_flashcard_reviews_card ON flashcard_reviews (flashcard_id, reviewed_at);
                ALTER TABLE users ADD COLUMN fsrs_retention REAL;
                ALTER TABLE users ADD COLUMN fsrs_weights TEXT;"#
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
                "DROP TABLE flashcard_reviews; DROP TABLE flashcards; \
                 ALTER TABLE users DROP COLUMN fsrs_retention; \
                 ALTER TABLE users DROP COLUMN fsrs_weights;"
                    .to_string(),
            ))
            .await?;
        Ok(())
    }
}
