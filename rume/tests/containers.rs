//! Container-backed live integration suites.

#![forbid(unsafe_code)]

#[path = "common/mod.rs"]
pub mod common;

#[path = "containers/postgres.rs"]
#[cfg(feature = "postgres")]
pub mod postgres;

#[path = "containers/mysql.rs"]
#[cfg(feature = "mysql")]
pub mod mysql;

#[path = "containers/redis.rs"]
#[cfg(feature = "redis")]
pub mod redis;

#[path = "containers/coordination.rs"]
#[cfg(feature = "redis")]
pub mod coordination;

#[path = "containers/jobs.rs"]
#[cfg(feature = "postgres")]
pub mod jobs;

#[path = "containers/job_reliability.rs"]
#[cfg(all(feature = "postgres", feature = "redis"))]
pub mod job_reliability;

#[path = "containers/pipeline.rs"]
#[cfg(feature = "postgres")]
pub mod pipeline;

#[path = "containers/crash_recovery.rs"]
#[cfg(all(feature = "postgres", feature = "redis"))]
pub mod crash_recovery;
