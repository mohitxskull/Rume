use axum::routing::Router;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::{BuildError, RumeBuilder};
use crate::app::context::AppContext;

/// Errors that can occur when building the middleware stack.
///
/// Rume fails closed: any reference to middleware that hasn't been registered,
/// any duplicate name, or any group containing an unknown member causes
/// [`build_router`](RumeBuilder::build_router) to return an error before the
/// application serves traffic. A misspelled middleware name must never
/// silently result in an unprotected route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiddlewareConfigError {
    /// The same middleware name was registered more than once.
    DuplicateMiddleware { name: String },
    /// The same group name was defined more than once.
    DuplicateGroup { name: String },
    /// `use_middleware` was called with a name that was never registered.
    UnknownMiddleware { name: String },
    /// `use_group` was called with a name that was never defined.
    UnknownGroup { name: String },
    /// A group lists a member that was never registered as middleware.
    UnknownGroupMember { group: String, member: String },
}

impl std::fmt::Display for MiddlewareConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MiddlewareConfigError::DuplicateMiddleware { name } => {
                write!(f, "middleware '{name}' is registered more than once")
            }
            MiddlewareConfigError::DuplicateGroup { name } => {
                write!(f, "middleware group '{name}' is defined more than once")
            }
            MiddlewareConfigError::UnknownMiddleware { name } => write!(
                f,
                "middleware '{name}' is marked for application but was never registered with `.middleware()`"
            ),
            MiddlewareConfigError::UnknownGroup { name } => write!(
                f,
                "middleware group '{name}' is marked for application but was never defined with `.group()`"
            ),
            MiddlewareConfigError::UnknownGroupMember { group, member } => write!(
                f,
                "middleware group '{group}' references unknown member '{member}' — register it with `.middleware()` first"
            ),
        }
    }
}

impl std::error::Error for MiddlewareConfigError {}

impl From<MiddlewareConfigError> for std::io::Error {
    fn from(err: MiddlewareConfigError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
    }
}

impl<E, C, S> RumeBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Register a named middleware closure by alias (AdonisJS-style).
    ///
    /// The closure wraps the router as a Tower layer. It is not applied yet:
    /// call [`use_middleware`](Self::use_middleware) to mark it for
    /// application. Applied middleware wrap the router in
    /// [`middleware_order`](Self::middleware_order) registration order.
    ///
    /// Duplicate names are detected when the router is built via
    /// [`build_router`](Self::build_router), which returns a
    /// [`MiddlewareConfigError::DuplicateMiddleware`] error.
    pub fn middleware<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(Router<AppContext<E, C, S>>) -> Router<AppContext<E, C, S>> + Send + Sync + 'static,
    {
        self.middleware_order.push(name.to_string());
        self.middleware.insert(name.to_string(), Box::new(f));
        self
    }

    pub fn use_middleware(mut self, name: &str) -> Self {
        self.applied_middleware.insert(name.to_string());
        self
    }

    pub fn group(mut self, name: &str, members: &[&str]) -> Self {
        self.groups_order.push(name.to_string());
        self.groups.insert(
            name.to_string(),
            members.iter().map(|member| member.to_string()).collect(),
        );
        self
    }

    pub fn use_group(mut self, name: &str) -> Self {
        self.applied_groups.insert(name.to_string());
        self
    }
}

pub(crate) fn validate_middleware_config<V>(
    middleware: &BTreeMap<String, V>,
    middleware_order: &[String],
    groups: &BTreeMap<String, Vec<String>>,
    groups_order: &[String],
    applied_middleware: &BTreeSet<String>,
    applied_groups: &BTreeSet<String>,
) -> Result<(), BuildError> {
    let mut seen_middleware: HashSet<&str> = HashSet::new();
    for name in middleware_order {
        if !seen_middleware.insert(name.as_str()) {
            return Err(MiddlewareConfigError::DuplicateMiddleware { name: name.clone() }.into());
        }
    }

    let mut seen_groups: HashSet<&str> = HashSet::new();
    for name in groups_order {
        if !seen_groups.insert(name.as_str()) {
            return Err(MiddlewareConfigError::DuplicateGroup { name: name.clone() }.into());
        }
    }

    for name in applied_middleware {
        if !middleware.contains_key(name.as_str()) {
            return Err(MiddlewareConfigError::UnknownMiddleware { name: name.clone() }.into());
        }
    }

    for name in applied_groups {
        if !groups.contains_key(name.as_str()) {
            return Err(MiddlewareConfigError::UnknownGroup { name: name.clone() }.into());
        }
    }

    // Why: validate every registered group's members (in declaration order),
    // not only `applied_groups`, matching the documented contract so unused
    // misconfigured groups fail closed at build/inspect time.
    for group_name in groups_order {
        let Some(members) = groups.get(group_name.as_str()) else {
            continue;
        };
        for member in members {
            if !middleware.contains_key(member.as_str()) {
                return Err(MiddlewareConfigError::UnknownGroupMember {
                    group: group_name.clone(),
                    member: member.clone(),
                }
                .into());
            }
        }
    }

    Ok(())
}
