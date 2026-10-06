pub use sea_orm_migration::prelude::*;

mod m20260101_000001_init;
mod m20260102_000002_lite_mode;
mod m20260103_000003_dictionary_groups;
mod m20260104_000004_flashcards;
mod m20260105_000005_fsrs6;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
        Box::new(m20260101_000001_init::Migration),
        Box::new(m20260102_000002_lite_mode::Migration),
        Box::new(m20260103_000003_dictionary_groups::Migration),
        Box::new(m20260104_000004_flashcards::Migration),
        Box::new(m20260105_000005_fsrs6::Migration),
    ]
    }
}
