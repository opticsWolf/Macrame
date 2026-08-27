// `ddl` stays public: it is the canonical home of twenty-two DDL constants,
// not a second path to them. `schema::CREATE_TRIGGERS` would say less than
// `schema::ddl::CREATE_TRIGGERS` does (D-208).
pub mod ddl;
pub(crate) mod migrations;
pub(crate) mod seed;

pub use migrations::{current_version, run as run_migrations, MigrationOutcome, SCHEMA_VERSION};
pub use seed::seed_bootstrap;
