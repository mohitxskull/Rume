use clap::{Parser, Subcommand, ValueEnum};

/// Supported database SQL dialects for newly scaffolded projects.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewProjectDialect {
    Sqlite,
    Postgres,
    Mysql,
}

#[derive(Parser)]
#[command(name = "rume", version, about = "Rume framework CLI")]
pub struct Cli {
    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scaffold a new Rume project.
    New {
        /// Project name.
        name: Option<String>,
        /// Path to local Rume framework crate (for local development or testing).
        #[arg(long)]
        path: Option<String>,
        /// Rume version to depend on (defaults to CLI version).
        #[arg(long)]
        version: Option<String>,
        /// Target SQL dialect for scaffolded query placeholders (sqlite, postgres, mysql).
        #[arg(long, value_enum, default_value_t = NewProjectDialect::Sqlite)]
        dialect: NewProjectDialect,
    },
    /// Inspect and validate environment/configuration.
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },
    /// Inspect and validate configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Validate the project setup.
    Doctor {
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List registered routes.
    #[command(name = "list:routes", alias = "list-routes")]
    ListRoutes {
        /// Name of the binary to run if the project has multiple binaries.
        #[arg(long)]
        bin: Option<String>,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output routes as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Regenerate barrel and registration files.
    Codegen {
        /// Show planned file modifications without writing to disk.
        #[arg(long)]
        dry_run: bool,
    },
    /// Generate a component.
    Generate {
        kind: Option<String>,
        name: Option<String>,
    },
    /// Create a new versioned database migration pair (.up.sql and .down.sql).
    #[command(name = "make:migration", alias = "make-migration")]
    MakeMigration {
        /// Migration name (e.g. create_users_table).
        name: Option<String>,
    },
    /// Database migration management.
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
    /// Shortcut for `rume db status`.
    #[command(name = "db:status", alias = "db-status", hide = true)]
    DbStatus,
    /// Shortcut for `rume db migrate`.
    #[command(name = "db:migrate", alias = "db-migrate", hide = true)]
    DbMigrate,
    /// Shortcut for `rume db rollback`.
    #[command(name = "db:rollback", alias = "db-rollback", hide = true)]
    DbRollback,
    /// Shortcut for `rume db seed`.
    #[command(name = "db:seed", alias = "db-seed", hide = true)]
    DbSeed {
        /// Directory containing `.sql` seed files (defaults to `seeds`).
        #[arg(long)]
        path: Option<String>,
        /// Comma-separated list of seed names to run.
        #[arg(long, value_delimiter = ',')]
        only: Option<Vec<String>>,
    },
    /// Validate SQL files in the project for syntax and non-empty queries.
    #[command(name = "sql:check", alias = "sql-check")]
    SqlCheck {
        /// Path to directory containing SQL files (defaults to `sql`).
        #[arg(long, default_value = "sql")]
        path: String,
        /// Target dialect to validate against (postgres, sqlite, mysql).
        #[arg(long)]
        dialect: Option<String>,
        /// Verify SQL statements against the live database via EXPLAIN.
        #[arg(long)]
        db: bool,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List or run registered application tasks.
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },
    /// Export OpenAPI v3.1 JSON or generate a typed TypeScript SDK client.
    Openapi {
        #[command(subcommand)]
        command: OpenapiCommands,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum OpenapiCommands {
    /// Export the application's OpenAPI v3.1 JSON specification.
    Export {
        /// Name of the binary to run if the project has multiple binaries.
        #[arg(long)]
        bin: Option<String>,
        /// Write output to a file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Generate typed TypeScript interfaces and a fetch client from OpenAPI JSON.
    Ts {
        /// Name of the binary to run if the project has multiple binaries.
        #[arg(long)]
        bin: Option<String>,
        /// Read OpenAPI JSON from a file instead of inspecting the project binary.
        #[arg(long)]
        input: Option<std::path::PathBuf>,
        /// Write generated TypeScript to a file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum TaskCommands {
    /// List registered application tasks.
    List {
        /// Name of the binary to run if the project has multiple binaries.
        #[arg(long)]
        bin: Option<String>,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output tasks as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run a registered application task.
    Run {
        /// Name of the task to execute.
        name: String,
        /// Name of the binary to run if the project has multiple binaries.
        #[arg(long)]
        bin: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum DbCommands {
    /// Inspect migration ledger and pending migrations.
    Status,
    /// Execute pending migrations against the configured database.
    Migrate,
    /// Rollback the most recently applied migration.
    Rollback,
    /// Execute database seed scripts.
    Seed {
        /// Directory containing `.sql` seed files (defaults to `seeds`).
        #[arg(long)]
        path: Option<String>,
        /// Comma-separated list of seed names to run.
        #[arg(long, value_delimiter = ',')]
        only: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Inspect resolved environment and configuration.
    Check {
        /// Include variables inherited from the process environment.
        #[arg(long)]
        all: bool,
        /// Show secret values in plaintext (default: redacted).
        #[arg(long)]
        unredacted: bool,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain how a single environment variable was resolved (secret values are always redacted).
    Explain {
        /// Environment variable name to explain.
        key: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print the merged configuration.
    Check {
        /// Show secret values in plaintext (default: redacted).
        #[arg(long)]
        unredacted: bool,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain how a configuration key was resolved and its source provenance.
    Explain {
        /// Config key to explain (e.g. app.name or database.url).
        key: String,
        /// Show secret values in plaintext without redaction.
        #[arg(long)]
        unredacted: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}
