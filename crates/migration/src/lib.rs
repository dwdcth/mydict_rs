pub use sea_orm_migration::prelude::*;

mod m20260101_000001_init;
mod m20260102_000002_lite_mode;
mod m20260103_000003_dictionary_groups;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
        Box::new(m20260101_000001_init::Migration),
        Box::new(m20260102_000002_lite_mode::Migration),
        Box::new(m20260103_000003_dictionary_groups::Migration),
    ]
    }
}
