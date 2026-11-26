//! Procedural macros for Rume persistence: SQL asset embedding and migration discovery.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token, Visibility, parse_macro_input};

mod openapi_schema;

struct SqlAssetsArgs {
    vis: Visibility,
    mod_name: Ident,
    dir_path: LitStr,
}

impl Parse for SqlAssetsArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(LitStr) {
            let dir_path: LitStr = input.parse()?;
            let vis = Visibility::Public(syn::token::Pub::default());
            let mod_name = Ident::new("queries", proc_macro2::Span::call_site());
            return Ok(Self {
                vis,
                mod_name,
                dir_path,
            });
        }

        let vis: Visibility = input.parse()?;
        let _mod_tok: Token![mod] = input.parse()?;
        let mod_name: Ident = input.parse()?;
        let from_ident: Ident = input.parse()?;
        if from_ident != "from" {
            return Err(syn::Error::new(
                from_ident.span(),
                "expected 'from', syntax: `pub mod <name> from \"<path>\";`",
            ));
        }
        let dir_path: LitStr = input.parse()?;
        let _semi: Option<Token![;]> = input.parse()?;

        Ok(Self {
            vis,
            mod_name,
            dir_path,
        })
    }
}

struct SqlFileItem {
    relative_path: String,
    dialect: Option<String>,
    namespace: String,
    query_ident: String,
    logical_name: String,
}

fn walk_sql_files(
    dir: &Path,
    span: proc_macro2::Span,
    out: &mut Vec<PathBuf>,
) -> Result<(), syn::Error> {
    let entries = fs::read_dir(dir).map_err(|e| {
        syn::Error::new(
            span,
            format!("sql_assets!: failed to read directory {dir:?}: {e}"),
        )
    })?;
    let mut sorted_entries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            syn::Error::new(
                span,
                format!("sql_assets!: failed to read directory entry in {dir:?}: {e}"),
            )
        })?;
        sorted_entries.push(entry);
    }
    sorted_entries.sort_by_key(|e| e.path());
    for entry in sorted_entries {
        let path = entry.path();
        if path.is_dir() {
            walk_sql_files(&path, span, out)?;
        } else if path.is_file() && path.extension().is_some_and(|ext| ext == "sql") {
            out.push(path);
        }
    }
    Ok(())
}

fn sanitize_ident(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{out}")
    } else {
        out
    }
}

/// Resolves `raw_rel` inside `manifest_dir`, rejecting path traversal (`..`,
/// absolute paths, and symlinks that escape the crate root).
///
/// Why: proc-macros run at compile time with the caller's `CARGO_MANIFEST_DIR`;
/// canonicalizing both `manifest_dir` and the target directory guarantees
/// neither `..` segments nor symlinks can read files outside the crate.
fn resolve_manifest_subdir(
    manifest_dir: &str,
    raw_rel: &str,
    macro_name: &str,
    span: proc_macro2::Span,
) -> Result<(PathBuf, PathBuf, String), syn::Error> {
    // Why: on Unix, `\` is a literal filename character rather than a path separator.
    // Rejecting backslashes on Unix prevents `Path::join` from validating a directory
    // literally named `sql\archive` while `normalized_rel` emits `sql/archive`.
    if cfg!(unix) && raw_rel.contains('\\') {
        return Err(syn::Error::new(
            span,
            format!("{macro_name}: path '{raw_rel}' must use '/' separators on Unix"),
        ));
    }

    let rel_path = Path::new(raw_rel);
    if raw_rel.is_empty()
        || rel_path.is_absolute()
        || raw_rel.starts_with('/')
        || raw_rel.starts_with('\\')
        || rel_path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(syn::Error::new(
            span,
            format!(
                "{macro_name}: path '{raw_rel}' must be a relative path within CARGO_MANIFEST_DIR without '..' components"
            ),
        ));
    }

    let full_dir = Path::new(manifest_dir).join(rel_path);
    if !full_dir.exists() {
        return Err(syn::Error::new(
            span,
            format!("{macro_name}: directory '{raw_rel}' not found at {full_dir:?}"),
        ));
    }

    let canonical_manifest = fs::canonicalize(manifest_dir).map_err(|e| {
        syn::Error::new(
            span,
            format!(
                "{macro_name}: failed to canonicalize CARGO_MANIFEST_DIR '{manifest_dir}': {e}"
            ),
        )
    })?;
    let canonical_dir = fs::canonicalize(&full_dir).map_err(|e| {
        syn::Error::new(
            span,
            format!("{macro_name}: failed to canonicalize directory {full_dir:?}: {e}"),
        )
    })?;

    if !canonical_dir.starts_with(&canonical_manifest) {
        return Err(syn::Error::new(
            span,
            format!("{macro_name}: directory '{raw_rel}' resolves outside CARGO_MANIFEST_DIR"),
        ));
    }

    let normalized_rel = raw_rel.replace('\\', "/").trim_end_matches('/').to_string();
    Ok((canonical_manifest, canonical_dir, normalized_rel))
}

fn verify_file_within_manifest(
    path: &Path,
    canonical_manifest: &Path,
    macro_name: &str,
    span: proc_macro2::Span,
) -> Result<(), syn::Error> {
    let canonical_file = fs::canonicalize(path).map_err(|e| {
        syn::Error::new(
            span,
            format!("{macro_name}: failed to canonicalize SQL file {path:?}: {e}"),
        )
    })?;
    if !canonical_file.starts_with(canonical_manifest) {
        return Err(syn::Error::new(
            span,
            format!("{macro_name}: SQL file {path:?} resolves outside CARGO_MANIFEST_DIR"),
        ));
    }
    Ok(())
}

/// Discovers, embeds, and registers compile-time SQL query assets.
///
/// # Usage
/// ```ignore
/// rume::sql_assets! { pub mod queries from "sql"; }
/// // Or:
/// rume::sql_assets!("sql");
/// ```
#[proc_macro]
pub fn sql_assets(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as SqlAssetsArgs);
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let (canonical_manifest, full_sql_dir, sql_dir_rel) = match resolve_manifest_subdir(
        &manifest_dir,
        &args.dir_path.value(),
        "sql_assets!",
        args.dir_path.span(),
    ) {
        Ok(res) => res,
        Err(err) => return err.to_compile_error().into(),
    };

    let mut sql_paths = Vec::new();
    if let Err(err) = walk_sql_files(&full_sql_dir, args.dir_path.span(), &mut sql_paths) {
        return err.to_compile_error().into();
    }

    let mut items = Vec::new();
    for path in sql_paths {
        if let Err(err) = verify_file_within_manifest(
            &path,
            &canonical_manifest,
            "sql_assets!",
            args.dir_path.span(),
        ) {
            return err.to_compile_error().into();
        }
        if let Ok(rel) = path.strip_prefix(&full_sql_dir) {
            let components: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            if components.is_empty() {
                continue;
            }

            let full_rel_from_manifest = format!(
                "{}/{}",
                sql_dir_rel,
                rel.to_string_lossy().replace('\\', "/")
            );

            let (dialect, remaining) = match components[0].as_str() {
                "postgres" | "sqlite" | "mysql" | "mariadb" => {
                    (Some(components[0].clone()), &components[1..])
                }
                _ => (None, &components[..]),
            };

            if remaining.is_empty() {
                continue;
            }

            let file_name = &remaining[remaining.len() - 1];
            let query_name = file_name.trim_end_matches(".sql");
            if syn::parse_str::<syn::Ident>(query_name).is_err() {
                return syn::Error::new(
                    args.dir_path.span(),
                    format!(
                        "sql_assets!: SQL filename '{file_name}' contains invalid Rust identifier '{query_name}'",
                    ),
                )
                .to_compile_error()
                .into();
            }
            let query_ident = sanitize_ident(query_name);

            let namespace = if remaining.len() > 1 {
                remaining[..remaining.len() - 1].join("_")
            } else {
                "common".to_string()
            };
            if syn::parse_str::<syn::Ident>(&namespace).is_err() {
                return syn::Error::new(
                    args.dir_path.span(),
                    format!(
                        "sql_assets!: subdirectory namespace '{namespace}' (from '{full_rel_from_manifest}') is not a valid Rust identifier",
                    ),
                )
                .to_compile_error()
                .into();
            }

            let logical_name = match &dialect {
                Some(d) => format!("{d}.{namespace}.{query_name}"),
                None => format!("{namespace}.{query_name}"),
            };

            items.push(SqlFileItem {
                relative_path: full_rel_from_manifest,
                dialect,
                namespace,
                query_ident,
                logical_name,
            });
        }
    }

    // Check for collision within the same dialect and namespace
    let mut seen_in_ns: BTreeSet<(Option<String>, String, String)> = BTreeSet::new();
    for item in &items {
        let key = (
            item.dialect.clone(),
            item.namespace.clone(),
            item.query_ident.clone(),
        );
        if !seen_in_ns.insert(key) {
            let dialect_label = item.dialect.as_deref().unwrap_or("common");
            return syn::Error::new(
                args.dir_path.span(),
                format!(
                    "sql_assets!: duplicate query identifier '{}' in dialect '{}', namespace '{}' (file: {})",
                    item.query_ident, dialect_label, item.namespace, item.relative_path
                ),
            )
            .to_compile_error()
            .into();
        }
    }

    // Organize items into dialect modules and flat namespace modules
    // 1. By dialect: dialect -> namespace -> Vec<Item>
    let mut by_dialect: BTreeMap<String, BTreeMap<String, Vec<&SqlFileItem>>> = BTreeMap::new();
    // 2. By namespace: namespace -> BTreeMap<query_ident, &SqlFileItem>
    let mut by_namespace: BTreeMap<String, BTreeMap<String, &SqlFileItem>> = BTreeMap::new();

    for item in &items {
        if let Some(dialect) = &item.dialect {
            by_dialect
                .entry(dialect.clone())
                .or_default()
                .entry(item.namespace.clone())
                .or_default()
                .push(item);
        }

        // For flat namespace access (e.g. `queries::issues::CREATE`):
        // If there is no dialect or if not yet registered, store it.
        let ns_map = by_namespace.entry(item.namespace.clone()).or_default();
        if !ns_map.contains_key(&item.query_ident) || item.dialect.is_none() {
            ns_map.insert(item.query_ident.clone(), item);
        }
    }

    // Generate dialect submodules
    let dialect_mods = by_dialect.iter().map(|(dialect_name, namespaces)| {
        let dialect_ident = Ident::new(dialect_name, proc_macro2::Span::call_site());
        let ns_mods = namespaces.iter().map(|(ns_name, query_items)| {
            let ns_ident = Ident::new(ns_name, proc_macro2::Span::call_site());
            let query_consts = query_items.iter().map(|q| {
                let const_ident = Ident::new(&q.query_ident, proc_macro2::Span::call_site());
                let logical_name = &q.logical_name;
                let rel_path = &q.relative_path;
                let dialect_expr = match &q.dialect {
                    Some(d) => quote! { ::core::option::Option::Some(#d) },
                    None => quote! { ::core::option::Option::None },
                };
                quote! {
                    pub const #const_ident: ::rume::persistence::SqlQuery<'static> =
                        ::rume::persistence::SqlQuery::new(
                            #logical_name,
                            #rel_path,
                            #dialect_expr,
                            ::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #rel_path)),
                        );
                }
            });
            quote! {
                pub mod #ns_ident {
                    #(#query_consts)*
                }
            }
        });
        quote! {
            pub mod #dialect_ident {
                #(#ns_mods)*
            }
        }
    });

    // Generate flat namespace submodules
    let flat_ns_mods = by_namespace.iter().map(|(ns_name, queries)| {
        let ns_ident = Ident::new(ns_name, proc_macro2::Span::call_site());
        let query_consts = queries.iter().map(|(q_ident_str, q)| {
            let const_ident = Ident::new(q_ident_str, proc_macro2::Span::call_site());
            let logical_name = &q.logical_name;
            let rel_path = &q.relative_path;
            let dialect_expr = match &q.dialect {
                Some(d) => quote! { ::core::option::Option::Some(#d) },
                None => quote! { ::core::option::Option::None },
            };
            quote! {
                pub const #const_ident: ::rume::persistence::SqlQuery<'static> =
                    ::rume::persistence::SqlQuery::new(
                        #logical_name,
                        #rel_path,
                        #dialect_expr,
                        ::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #rel_path)),
                    );
            }
        });
        quote! {
            pub mod #ns_ident {
                #(#query_consts)*
            }
        }
    });

    let vis = &args.vis;
    let mod_name = &args.mod_name;

    let expanded = quote! {
        #vis mod #mod_name {
            #(#dialect_mods)*
            #(#flat_ns_mods)*
        }
    };

    expanded.into()
}

fn extract_migration_version(id: &str) -> &str {
    let parts: Vec<&str> = id.split('_').collect();
    let mut version_parts_len = 0;
    for part in &parts {
        // Why: only purely-numeric underscore-separated parts count as version
        // segments. `.any(|c| c.is_ascii_digit())` treated mixed segments like
        // "001a" or "v2" as version parts because they contain a digit. `.all()`
        // requires every character in the segment to be an ASCII digit, and
        // returning "" when no leading segment is purely numeric ensures
        // "001a_add_users" is never treated as version "001a".
        if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) {
            version_parts_len += 1;
        } else {
            break;
        }
    }
    if version_parts_len > 0 {
        let end_idx = parts[..version_parts_len]
            .iter()
            .map(|p| p.len())
            .sum::<usize>()
            + (version_parts_len - 1);
        &id[..end_idx]
    } else {
        ""
    }
}

/// Discovers `.up.sql` and `.down.sql` migrations and embeds them into a `Migrator`.
///
/// # Usage
/// ```ignore
/// let migrator = rume::embed_migrations!("migrations").with_rume_core("postgres");
/// ```
#[proc_macro]
pub fn embed_migrations(input: TokenStream) -> TokenStream {
    let dir_lit = parse_macro_input!(input as LitStr);
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let (canonical_manifest, full_dir, dir_rel) = match resolve_manifest_subdir(
        &manifest_dir,
        &dir_lit.value(),
        "embed_migrations!",
        dir_lit.span(),
    ) {
        Ok(res) => res,
        Err(err) => return err.to_compile_error().into(),
    };

    let mut up_files: BTreeMap<String, String> = BTreeMap::new();
    let mut down_files: BTreeSet<String> = BTreeSet::new();
    let mut seen_versions: BTreeMap<String, String> = BTreeMap::new();

    let entries = match fs::read_dir(&full_dir) {
        Ok(e) => e,
        Err(err) => {
            return syn::Error::new(
                dir_lit.span(),
                format!(
                    "embed_migrations!: failed to read directory {:?}: {err}",
                    full_dir
                ),
            )
            .to_compile_error()
            .into();
        }
    };

    let mut entries_vec = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                return syn::Error::new(
                    dir_lit.span(),
                    format!(
                        "embed_migrations!: failed to read entry in {:?}: {err}",
                        full_dir
                    ),
                )
                .to_compile_error()
                .into();
            }
        };
        entries_vec.push(entry);
    }
    entries_vec.sort_by_key(|e| e.path());

    for entry in entries_vec {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') {
            continue;
        }

        if path.extension().is_some_and(|ext| ext == "sql") {
            if let Err(err) = verify_file_within_manifest(
                &path,
                &canonical_manifest,
                "embed_migrations!",
                dir_lit.span(),
            ) {
                return err.to_compile_error().into();
            }
            let is_up = file_name.ends_with(".up.sql");
            let is_down = file_name.ends_with(".down.sql");
            if !is_up && !is_down {
                return syn::Error::new(
                    dir_lit.span(),
                    format!(
                        "embed_migrations!: malformed migration filename '{file_name}'. Expected format: '<version>_<name>.up.sql' or '<version>_<name>.down.sql'"
                    ),
                )
                .to_compile_error()
                .into();
            }

            let id = if is_up {
                file_name.strip_suffix(".up.sql").unwrap()
            } else {
                file_name.strip_suffix(".down.sql").unwrap()
            };

            let Some((version, name)) = id.split_once('_') else {
                return syn::Error::new(
                    dir_lit.span(),
                    format!(
                        "embed_migrations!: malformed migration filename '{file_name}'. Expected format: '<version>_<name>.up.sql'"
                    ),
                )
                .to_compile_error()
                .into();
            };

            if version.is_empty() || name.is_empty() {
                return syn::Error::new(
                    dir_lit.span(),
                    format!(
                        "embed_migrations!: malformed migration filename '{file_name}'. Expected format: '<version>_<name>.up.sql'"
                    ),
                )
                .to_compile_error()
                .into();
            }

            if is_up {
                let ver_prefix = extract_migration_version(id);
                if !ver_prefix.is_empty()
                    && let Some(existing_file) =
                        seen_versions.insert(ver_prefix.to_string(), file_name.to_string())
                {
                    return syn::Error::new(
                        dir_lit.span(),
                        format!(
                            "embed_migrations!: duplicate migration version prefix '{ver_prefix}' found in '{file_name}' and '{existing_file}'"
                        ),
                    )
                    .to_compile_error()
                    .into();
                }
                let rel_path = format!("{}/{}", dir_rel.replace('\\', "/"), file_name);
                up_files.insert(id.to_string(), rel_path);
            } else {
                down_files.insert(id.to_string());
            }
        }
    }

    let migration_calls = up_files.iter().map(|(id, up_rel_path)| {
        let desc = id.replace('_', " ");
        if down_files.contains(id) {
            let down_rel_path = format!("{}/{}.down.sql", dir_rel.replace('\\', "/"), id);
            quote! {
                migrator = migrator.add_migration(
                    ::rume::persistence::Migration::from_sql(
                        #id,
                        #desc,
                        ::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #up_rel_path)),
                    )
                    .with_down(::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #down_rel_path)))
                );
            }
        } else {
            quote! {
                migrator = migrator.add_migration(
                    ::rume::persistence::Migration::from_sql(
                        #id,
                        #desc,
                        ::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #up_rel_path)),
                    )
                );
            }
        }
    });

    let expanded = quote! {
        {
            let mut migrator = ::rume::persistence::Migrator::new();
            #(#migration_calls)*
            migrator
        }
    };

    expanded.into()
}

/// Derives `HasOpenApiSchema` for a struct, reflecting field types, `Option<T>`
/// optionality, `#[serde(...)]` attributes, and common `#[garde(...)]` rules.
#[proc_macro_derive(HasOpenApiSchema, attributes(openapi, serde, garde))]
pub fn derive_has_openapi_schema(input: TokenStream) -> TokenStream {
    openapi_schema::expand(input)
}

#[cfg(test)]
mod tests {
    use super::{extract_migration_version, resolve_manifest_subdir};

    #[test]
    fn extract_migration_version_requires_all_digit_segments() {
        assert_eq!(extract_migration_version("001_add_users"), "001");
        assert_eq!(
            extract_migration_version("20261118_114500_create_users"),
            "20261118_114500"
        );
        assert_eq!(extract_migration_version("001_oauth2_tokens"), "001");
        assert_eq!(extract_migration_version("001_v2_add_users"), "001");
        assert_ne!(extract_migration_version("001a_add_users"), "001a");
        assert_eq!(extract_migration_version("001a_add_users"), "");
        assert_eq!(extract_migration_version("_add_users"), "");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_manifest_subdir_rejects_backslash_on_unix() {
        let dir =
            std::env::temp_dir().join(format!("rume_macro_backslash_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sql/archive")).unwrap();
        std::fs::create_dir_all(dir.join(r"sql\archive")).unwrap();
        std::fs::write(dir.join("sql/archive/q.sql"), "SELECT 1;").unwrap();
        std::fs::write(dir.join(r"sql\archive/q.sql"), "SELECT 2;").unwrap();

        let err = resolve_manifest_subdir(
            dir.to_str().unwrap(),
            r"sql\archive",
            "sql_assets!",
            proc_macro2::Span::call_site(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("must use '/' separators on Unix"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
