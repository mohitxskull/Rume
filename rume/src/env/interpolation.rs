//! Environment variable interpolation.
//!
//! Provides recursive `${VAR}` interpolation with circular-reference
//! detection, escaping (`\${VAR}`), and a maximum recursion depth.

use std::collections::{BTreeMap, HashSet};

use crate::env::error::EnvError;

/// Maximum recursion depth for interpolating `${VAR}` references, guarding
/// against infinitely recursive or circular references.
const MAX_INTERPOLATION_DEPTH: usize = 10;

/// Interpolate `${VAR}` references in `value` using the merged variable map.
///
/// - `\${VAR}` is escaped to a literal `${VAR}` (no substitution).
/// - References are resolved recursively: if a substituted value itself
///   contains `${...}`, it is resolved again.
/// - Missing variables return [`EnvError::Interpolation`].
/// - Circular references return [`EnvError::CircularReference`].
/// - Exceeding maximum recursion depth returns [`EnvError::MaxDepthExceeded`].
pub fn interpolate_value(value: &str, vars: &BTreeMap<String, String>) -> Result<String, EnvError> {
    interpolate_recursive(value, vars, &mut HashSet::new(), 0, None)
}

fn interpolate_recursive(
    value: &str,
    vars: &BTreeMap<String, String>,
    visiting: &mut HashSet<String>,
    depth: usize,
    current_var: Option<&str>,
) -> Result<String, EnvError> {
    if depth > MAX_INTERPOLATION_DEPTH {
        return Err(EnvError::MaxDepthExceeded {
            variable: current_var.unwrap_or("<root>").to_string(),
            depth: MAX_INTERPOLATION_DEPTH,
        });
    }

    let mut out = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '$' {
            // Escaped substitution: \${VAR} -> literal ${VAR}
            if let Some((end, var)) = read_substitution(&chars[i + 1..]) {
                out.push_str("${");
                out.push_str(&var);
                out.push('}');
                i += 1 + end;
                continue;
            }
            out.push('\\');
            i += 1;
            continue;
        }

        if chars[i] == '$'
            && i + 1 < chars.len()
            && chars[i + 1] == '{'
            && let Some((end, var)) = read_substitution(&chars[i..])
        {
            if visiting.contains(&var) {
                return Err(EnvError::CircularReference {
                    variable: var.clone(),
                });
            }
            let resolved = match vars.get(&var) {
                Some(v) => v,
                None => {
                    return Err(EnvError::Interpolation {
                        variable: var.clone(),
                    });
                }
            };
            visiting.insert(var.clone());
            let expanded = interpolate_recursive(resolved, vars, visiting, depth + 1, Some(&var));
            visiting.remove(&var);
            out.push_str(&expanded?);
            i += end;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    Ok(out)
}

/// Read a `${VAR}` substitution starting at `chars[0]` (which must be `$`).
///
/// Returns the index (relative to `chars`) just after the closing `}` and the
/// variable name. Returns `None` if the text is not a well-formed substitution.
fn read_substitution(chars: &[char]) -> Option<(usize, String)> {
    if chars.len() < 2 || chars[0] != '$' || chars[1] != '{' {
        return None;
    }

    let mut var = String::new();
    let mut i = 2;
    while i < chars.len() {
        if chars[i] == '}' {
            return Some((i + 1, var));
        }
        if chars[i].is_alphanumeric() || chars[i] == '_' {
            var.push(chars[i]);
        } else {
            return None;
        }
        i += 1;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_basic_variable() {
        let vars = BTreeMap::from([("DATABASE_HOST".to_string(), "localhost".to_string())]);
        assert_eq!(
            interpolate_value("postgres://${DATABASE_HOST}:5432/app", &vars).unwrap(),
            "postgres://localhost:5432/app"
        );
    }

    #[test]
    fn interpolates_escaped_variable() {
        let vars = BTreeMap::from([("NOT_AN_ENV_VARIABLE".to_string(), "ignored".to_string())]);
        // \${VAR} is a literal, no substitution.
        assert_eq!(
            interpolate_value("\\${NOT_AN_ENV_VARIABLE}", &vars).unwrap(),
            "${NOT_AN_ENV_VARIABLE}"
        );
    }

    #[test]
    fn interpolates_recursive_references() {
        let vars = BTreeMap::from([
            ("A".to_string(), "${B}".to_string()),
            ("B".to_string(), "${C}".to_string()),
            ("C".to_string(), "hello".to_string()),
        ]);
        assert_eq!(interpolate_value("${A}", &vars).unwrap(), "hello");
    }

    #[test]
    fn interpolates_missing_variable_errors() {
        let vars = BTreeMap::new();
        let err = interpolate_value("${MISSING}", &vars).unwrap_err();
        assert!(matches!(err, EnvError::Interpolation { .. }));
        assert_eq!(
            err.to_string(),
            "failed to interpolate ${MISSING}: variable not found"
        );
    }

    #[test]
    fn interpolates_circular_reference_errors() {
        let vars = BTreeMap::from([
            ("A".to_string(), "${B}".to_string()),
            ("B".to_string(), "${A}".to_string()),
        ]);
        let err = interpolate_value("${A}", &vars).unwrap_err();
        assert!(matches!(err, EnvError::CircularReference { .. }));
        assert_eq!(
            err.to_string(),
            "circular reference detected while interpolating ${A}"
        );
    }

    #[test]
    fn deep_nesting_exceeding_max_depth_does_not_disclose_secret() {
        let secret = "super-secret-token-xyz-123";
        let mut vars = BTreeMap::new();
        // Create a chain of 15 variables: V0 -> V1 -> ... -> V14 -> secret
        for i in 0..14 {
            vars.insert(format!("V{i}"), format!("${{V{}}}", i + 1));
        }
        vars.insert("V14".to_string(), secret.to_string());

        let err = interpolate_value("${V0}", &vars).unwrap_err();
        assert!(matches!(err, EnvError::MaxDepthExceeded { .. }));

        let err_msg = err.to_string();
        // The error message must NEVER disclose the secret value
        assert!(
            !err_msg.contains(secret),
            "error message disclosed secret value: {err_msg}"
        );
        // The error message identifies the variable name, not the value
        assert!(err_msg.contains("maximum interpolation depth"));
    }

    #[test]
    fn interpolates_leaves_unknown_brace_content_intact() {
        let vars = BTreeMap::new();
        // `${not-valid}` (hyphen) is not a valid variable name, left as-is.
        assert_eq!(
            interpolate_value("${not-valid}", &vars).unwrap(),
            "${not-valid}"
        );
    }
}
