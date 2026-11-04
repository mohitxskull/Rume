use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rume", version, about = "Rume framework CLI")]
pub struct Cli {
    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Enable verbose output.
    #[arg(long, global = true)]
    pub verbose: bool,
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
}

#[derive(Subcommand, Debug, Clone)]
pub enum DbCommands {
    /// Inspect migration ledger and pending migrations.
    Status,
    /// Execute pending migrations against the configured database.
    Migrate,
    /// Rollback the most recently applied migration.
    Rollback,
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
    /// Explain how a single environment variable was resolved.
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
