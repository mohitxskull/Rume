//! Environment variable interpolation.
//!
//! Provides recursive `${VAR}` interpolation with circular-reference
//! detection, escaping (`\${VAR}`), and a maximum recursion depth.

use std::collections::{BTreeMap, HashSet};

use crate::env::error::EnvError;

/// Maximum recursion depth for interpolating `${VAR}` references, guarding
/// against infinitely recursive or circular references.
const MAX_INTERPOLATION_DEPTH: usize = 10;

/// Maximum total number of `${VAR}` expansions permitted across a single
/// [`interpolate_value`] call, preventing exponential DAG blowup.
const MAX_INTERPOLATION_EXPANSIONS: usize = 1024;

/// Maximum total output size in bytes permitted for a single
/// [`interpolate_value`] call, preventing memory exhaustion.
const MAX_INTERPOLATION_OUTPUT_BYTES: usize = 64 * 1024;

struct InterpolationBudget {
    remaining_expansions: usize,
    remaining_output_bytes: usize,
}

impl InterpolationBudget {
    fn new() -> Self {
        Self {
            remaining_expansions: MAX_INTERPOLATION_EXPANSIONS,
            remaining_output_bytes: MAX_INTERPOLATION_OUTPUT_BYTES,
        }
    }

    fn consume_expansion(&mut self, variable: &str) -> Result<(), EnvError> {
        if self.remaining_expansions == 0 {
            return Err(EnvError::BudgetExceeded {
                variable: variable.to_string(),
                reason: format!(
                    "maximum expansion count ({MAX_INTERPOLATION_EXPANSIONS}) exceeded"
                ),
            });
        }
        self.remaining_expansions -= 1;
        Ok(())
    }

    fn consume_bytes(&mut self, bytes: usize, current_var: Option<&str>) -> Result<(), EnvError> {
        if bytes > self.remaining_output_bytes {
            return Err(EnvError::BudgetExceeded {
                variable: current_var.unwrap_or("<root>").to_string(),
                reason: format!(
                    "maximum output size ({MAX_INTERPOLATION_OUTPUT_BYTES} bytes) exceeded"
                ),
            });
        }
        self.remaining_output_bytes -= bytes;
        Ok(())
    }
}

/// Interpolate `${VAR}` references in `value` using the merged variable map.
///
/// - `\${VAR}` is escaped to a literal `${VAR}` (no substitution).
/// - References are resolved recursively: if a substituted value itself
///   contains `${...}`, it is resolved again.
/// - Missing variables return [`EnvError::Interpolation`].
/// - Circular references return [`EnvError::CircularReference`].
/// - Exceeding maximum recursion depth returns [`EnvError::MaxDepthExceeded`].
/// - Exceeding expansion count or output byte budgets returns [`EnvError::BudgetExceeded`].
pub fn interpolate_value(value: &str, vars: &BTreeMap<String, String>) -> Result<String, EnvError> {
    let mut budget = InterpolationBudget::new();
    interpolate_recursive(value, vars, &mut HashSet::new(), 0, None, &mut budget)
}

fn interpolate_recursive(
    value: &str,
    vars: &BTreeMap<String, String>,
    visiting: &mut HashSet<String>,
    depth: usize,
    current_var: Option<&str>,
    budget: &mut InterpolationBudget,
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
                budget.consume_bytes(3 + var.len(), current_var)?;
                out.push_str("${");
                out.push_str(&var);
                out.push('}');
                i += 1 + end;
                continue;
            }
            budget.consume_bytes(1, current_var)?;
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
            // Why: check depth before consuming expansion budget so linear chains deeper than
            // `MAX_INTERPOLATION_DEPTH` report `MaxDepthExceeded`, while wide DAGs within depth
            // are stopped by `BudgetExceeded` before exponential work occurs.
            if depth + 1 > MAX_INTERPOLATION_DEPTH {
                return Err(EnvError::MaxDepthExceeded {
                    variable: var,
                    depth: MAX_INTERPOLATION_DEPTH,
                });
            }
            budget.consume_expansion(&var)?;
            visiting.insert(var.clone());
            let expanded =
                interpolate_recursive(resolved, vars, visiting, depth + 1, Some(&var), budget);
            visiting.remove(&var);
            out.push_str(&expanded?);
            i += end;
            continue;
        }

        budget.consume_bytes(chars[i].len_utf8(), current_var)?;
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

    #[test]
    fn diamond_dag_exceeding_expansion_budget_errors_quickly() {
        let mut vars = BTreeMap::new();
        // 10-level (depth 1..=10) 4-way diamond DAG: L0 -> 4x L1 -> ... -> L8 -> 4x L9
        // (4^9 = 262,144 leaves, all within MAX_INTERPOLATION_DEPTH = 10).
        for level in 0..9 {
            let next = level + 1;
            vars.insert(
                format!("L{level}"),
                format!("${{L{next}}}${{L{next}}}${{L{next}}}${{L{next}}}"),
            );
        }
        vars.insert("L9".to_string(), "x".to_string());

        let err = interpolate_value("${L0}", &vars)
            .expect_err("10-level 4-way diamond DAG must fail on expansion budget");
        assert!(
            matches!(err, EnvError::BudgetExceeded { .. }),
            "expected BudgetExceeded, got: {err:?}"
        );
    }

    #[test]
    fn output_byte_budget_prevents_memory_blowup() {
        let chunk = "a".repeat(2048);
        let mut vars = BTreeMap::new();
        vars.insert("CHUNK".to_string(), chunk);
        // 40 * 2048 = 81,920 bytes > 64 KiB, with only 40 expansions (< 1024).
        let template = "${CHUNK}".repeat(40);
        let err = interpolate_value(&template, &vars)
            .expect_err("output exceeding MAX_INTERPOLATION_OUTPUT_BYTES must fail");
        assert!(
            matches!(err, EnvError::BudgetExceeded { .. }),
            "expected BudgetExceeded, got: {err:?}"
        );
        assert!(err.to_string().contains("output size"));
    }
}

#[cfg(test)]
mod interpolate_value_proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn interpolate_value_never_panics_on_arbitrary_utf8(
            input in "\\PC*",
            vars in prop::collection::btree_map("[a-zA-Z0-9_]{1,12}", "\\PC{0,32}", 0..8),
        ) {
            let _ = interpolate_value(&input, &vars);
        }

        #[test]
        fn interpolate_value_escaped_dollar_brace_roundtrips_regardless_of_vars(
            var in "[a-zA-Z0-9_]{1,16}",
            secret in "[a-zA-Z0-9_!@#]{4,24}",
            prefix in "[a-zA-Z0-9_ ]{0,16}",
            suffix in "[a-zA-Z0-9_ ]{0,16}",
        ) {
            let vars = BTreeMap::from([(var.clone(), secret)]);
            let input = format!("{prefix}\\${{{var}}}{suffix}");
            let expected = format!("{prefix}${{{var}}}{suffix}");
            let actual = interpolate_value(&input, &vars)
                .expect("escaped substitution must always succeed");
            prop_assert_eq!(actual, expected);
        }

        #[test]
        fn interpolate_value_acyclic_chains_respect_max_depth_without_leaking_values(
            depth in 1usize..=MAX_INTERPOLATION_DEPTH,
            excess in 1usize..=5,
            leaf in "[a-zA-Z0-9_ ]{8,24}",
        ) {
            let mut valid_vars = BTreeMap::new();
            for i in 0..depth.saturating_sub(1) {
                valid_vars.insert(format!("K{i}"), format!("${{K{}}}", i + 1));
            }
            valid_vars.insert(format!("K{}", depth - 1), leaf.clone());
            prop_assert_eq!(
                interpolate_value("${K0}", &valid_vars).expect("within MAX_INTERPOLATION_DEPTH"),
                leaf.clone()
            );

            let too_deep = MAX_INTERPOLATION_DEPTH + excess;
            let mut deep_vars = BTreeMap::new();
            for i in 0..too_deep {
                deep_vars.insert(format!("D{i}"), format!("${{D{}}}", i + 1));
            }
            deep_vars.insert(format!("D{too_deep}"), leaf.clone());
            let err = interpolate_value("${D0}", &deep_vars)
                .expect_err("chain exceeding MAX_INTERPOLATION_DEPTH must fail");
            let is_max_depth = matches!(
                &err,
                EnvError::MaxDepthExceeded { variable, depth }
                    if variable == &format!("D{MAX_INTERPOLATION_DEPTH}")
                        && *depth == MAX_INTERPOLATION_DEPTH
            );
            prop_assert!(is_max_depth, "expected MaxDepthExceeded, got: {err:?}");
            let expected = format!(
                "maximum interpolation depth ({MAX_INTERPOLATION_DEPTH}) exceeded while interpolating ${{D{MAX_INTERPOLATION_DEPTH}}}"
            );
            prop_assert_eq!(err.to_string(), expected);
        }

        #[test]
        fn interpolate_value_cycles_fail_closed_without_leaking_values(
            cycle_len in 1usize..=5,
            secret in "[a-zA-Z0-9_]{8,24}",
        ) {
            let mut vars = BTreeMap::new();
            for i in 0..cycle_len.saturating_sub(1) {
                vars.insert(format!("C{i}"), format!("${{C{}}}", i + 1));
            }
            vars.insert(format!("C{}", cycle_len - 1), format!("{secret}${{C0}}"));

            let err = interpolate_value("${C0}", &vars)
                .expect_err("cyclic chain must fail closed");
            let is_circular = matches!(
                &err,
                EnvError::CircularReference { variable } if variable == "C0"
            );
            prop_assert!(is_circular, "expected CircularReference, got: {err:?}");
            prop_assert_eq!(
                err.to_string(),
                "circular reference detected while interpolating ${C0}"
            );
        }
    }
}
