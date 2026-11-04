//! Procedural macros for Rume persistence: SQL asset embedding and migration discovery.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Ident, LitStr, Token, Visibility};

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

fn walk_sql_files(dir: &Path, span: proc_macro2::Span, out: &mut Vec<PathBuf>) -> Result<(), syn::Error> {
    let entries = fs::read_dir(dir).map_err(|e| {
        syn::Error::new(span, format!("sql_assets!: failed to read directory {dir:?}: {e}"))
    })?;
    let mut sorted_entries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            syn::Error::new(span, format!("sql_assets!: failed to read directory entry in {dir:?}: {e}"))
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
    let sql_dir_rel = args.dir_path.value();
    let sql_dir_rel = sql_dir_rel.trim_start_matches('/').trim_start_matches('\\');
    let full_sql_dir = Path::new(&manifest_dir).join(sql_dir_rel);

    if !full_sql_dir.exists() {
        let err_msg = format!(
            "sql_assets!: directory '{}' not found at {:?}",
            args.dir_path.value(),
            full_sql_dir
        );
        return syn::Error::new(args.dir_path.span(), err_msg)
            .to_compile_error()
            .into();
    }

    let mut sql_paths = Vec::new();
    if let Err(err) = walk_sql_files(&full_sql_dir, args.dir_path.span(), &mut sql_paths) {
        return err.to_compile_error().into();
    }

    let mut items = Vec::new();
    for path in sql_paths {
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
                sql_dir_rel.replace('\\', "/"),
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
            let query_ident = sanitize_ident(query_name);

            let namespace = if remaining.len() > 1 {
                remaining[..remaining.len() - 1].join("_")
            } else {
                "common".to_string()
            };

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
        let key = (item.dialect.clone(), item.namespace.clone(), item.query_ident.clone());
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
                    Some(d) => quote! { Some(#d) },
                    None => quote! { None },
                };
                quote! {
                    pub const #const_ident: ::rume::persistence::SqlQuery<'static> =
                        ::rume::persistence::SqlQuery::new(
                            #logical_name,
                            #rel_path,
                            #dialect_expr,
                            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #rel_path)),
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
                Some(d) => quote! { Some(#d) },
                None => quote! { None },
            };
            quote! {
                pub const #const_ident: ::rume::persistence::SqlQuery<'static> =
                    ::rume::persistence::SqlQuery::new(
                        #logical_name,
                        #rel_path,
                        #dialect_expr,
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #rel_path)),
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

/// Discovers `.up.sql` and `.down.sql` migrations and embeds them into a [`Migrator`].
///
/// # Usage
/// ```ignore
/// let migrator = rume::embed_migrations!("migrations").with_rume_core("postgres");
/// ```
#[proc_macro]
pub fn embed_migrations(input: TokenStream) -> TokenStream {
    let dir_lit = parse_macro_input!(input as LitStr);
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let dir_rel = dir_lit.value();
    let dir_rel = dir_rel.trim_start_matches('/').trim_start_matches('\\');
    let full_dir = Path::new(&manifest_dir).join(dir_rel);

    if !full_dir.exists() {
        let err_msg = format!(
            "embed_migrations!: directory '{}' not found at {:?}",
            dir_lit.value(),
            full_dir
        );
        return syn::Error::new(dir_lit.span(), err_msg)
            .to_compile_error()
            .into();
    }

    let mut up_files: BTreeMap<String, String> = BTreeMap::new();
    let mut down_files: BTreeSet<String> = BTreeSet::new();

    let entries = match fs::read_dir(&full_dir) {
        Ok(e) => e,
        Err(err) => {
            return syn::Error::new(
                dir_lit.span(),
                format!("embed_migrations!: failed to read directory {:?}: {err}", full_dir),
            )
            .to_compile_error()
            .into();
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                return syn::Error::new(
                    dir_lit.span(),
                    format!("embed_migrations!: failed to read entry in {:?}: {err}", full_dir),
                )
                .to_compile_error()
                .into();
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(id) = file_name.strip_suffix(".up.sql") {
            let rel_path = format!("{}/{}", dir_rel.replace('\\', "/"), file_name);
            up_files.insert(id.to_string(), rel_path);
        } else if let Some(id) = file_name.strip_suffix(".down.sql") {
            down_files.insert(id.to_string());
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
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #up_rel_path)),
                    )
                    .with_down(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #down_rel_path)))
                );
            }
        } else {
            quote! {
                migrator = migrator.add_migration(
                    ::rume::persistence::Migration::from_sql(
                        #id,
                        #desc,
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #up_rel_path)),
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
