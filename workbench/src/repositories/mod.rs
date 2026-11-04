rume::sql_assets! { pub mod queries from "sql"; }

pub mod issues;
pub mod organizations;
pub mod projects;
pub mod users;

pub use issues::IssueRepository;
pub use organizations::OrganizationRepository;
pub use projects::ProjectRepository;
pub use users::UserRepository;
