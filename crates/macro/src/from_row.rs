//! Implementation of `#[derive(FromRow)]`.

use std::collections::BTreeSet;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Fields, LitStr, Path, parse_quote};

use crate::openapi_schema::{collect_used_type_params, is_option_type};

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    match syn::parse::<DeriveInput>(input).and_then(expand_derive) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[derive(Default)]
struct FieldAttrs {
    rename: Option<String>,
    default: bool,
    skip: bool,
    flatten: bool,
    json: bool,
    attr_span: Option<Span>,
}

fn expand_derive(input: DeriveInput) -> syn::Result<TokenStream2> {
    let crate_path = parse_from_row_crate_path(&input.attrs)?;

    let named_fields = match &input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(fields) => Some(&fields.named),
            Fields::Unit => None,
            Fields::Unnamed(fields) => {
                return Err(syn::Error::new_spanned(
                    fields,
                    "FromRow cannot be derived for tuple structs; use named fields instead",
                ));
            }
        },
        Data::Union(data_union) => {
            return Err(syn::Error::new(
                data_union.union_token.span,
                "FromRow cannot be derived for unions; only structs with named fields are supported",
            ));
        }
        Data::Enum(data_enum) => {
            return Err(syn::Error::new(
                data_enum.enum_token.span,
                "FromRow cannot be derived for enums; only structs with named fields are supported",
            ));
        }
    };

    let type_param_names: BTreeSet<String> = input
        .generics
        .type_params()
        .map(|tp| tp.ident.to_string())
        .collect();

    let mut from_db_value_params = BTreeSet::new();
    let mut from_row_params = BTreeSet::new();
    let mut json_params = BTreeSet::new();
    let mut default_params = BTreeSet::new();
    let mut field_inits = Vec::new();

    if let Some(fields) = named_fields {
        for field in fields {
            let Some(field_ident) = &field.ident else {
                continue;
            };
            let attrs = parse_field_attrs(&field.attrs)?;
            let field_ty = &field.ty;

            let raw_name = field_ident.to_string();
            let base_name = raw_name.strip_prefix("r#").unwrap_or(&raw_name);
            let col_name = attrs
                .rename
                .clone()
                .unwrap_or_else(|| base_name.to_string());

            if attrs.default || attrs.skip {
                collect_used_type_params(field_ty, &type_param_names, &mut default_params);
            }

            if attrs.skip {
                field_inits.push(quote! {
                    #field_ident: ::core::default::Default::default(),
                });
                continue;
            }

            if attrs.flatten {
                collect_used_type_params(field_ty, &type_param_names, &mut from_row_params);
                field_inits.push(quote! {
                    #field_ident: <#field_ty as #crate_path::FromRow>::from_row(row)?,
                });
                continue;
            }

            if attrs.json {
                collect_used_type_params(field_ty, &type_param_names, &mut json_params);
                let null_arm = if attrs.default || is_option_type(field_ty) {
                    quote! { ::core::default::Default::default() }
                } else {
                    quote! {
                        return ::core::result::Result::Err(
                            #crate_path::PersistenceError::DecodeError {
                                column: #col_name.to_string(),
                                message: ::std::string::String::from(
                                    "cannot deserialize NULL database value as JSON",
                                ),
                            }
                        )
                    }
                };
                let missing_arm = if attrs.default {
                    quote! {
                        ::core::result::Result::Err(#crate_path::PersistenceError::ColumnNotFound(_)) => {
                            ::core::default::Default::default()
                        }
                    }
                } else {
                    quote! {}
                };

                field_inits.push(quote! {
                    #field_ident: match row.get_value(#col_name) {
                        #missing_arm
                        ::core::result::Result::Err(err) => {
                            return ::core::result::Result::Err(err);
                        }
                        ::core::result::Result::Ok(#crate_path::DbValue::Null) => {
                            #null_arm
                        }
                        ::core::result::Result::Ok(#crate_path::DbValue::Text(s)) => {
                            ::serde_json::from_str::<#field_ty>(s).map_err(|e| {
                                #crate_path::PersistenceError::DecodeError {
                                    column: #col_name.to_string(),
                                    message: ::std::format!(
                                        "failed to deserialize JSON from column '{}': {e}",
                                        #col_name
                                    ),
                                }
                            })?
                        }
                        ::core::result::Result::Ok(#crate_path::DbValue::Bytes(b)) => {
                            ::serde_json::from_slice::<#field_ty>(b).map_err(|e| {
                                #crate_path::PersistenceError::DecodeError {
                                    column: #col_name.to_string(),
                                    message: ::std::format!(
                                        "failed to deserialize JSON from column '{}': {e}",
                                        #col_name
                                    ),
                                }
                            })?
                        }
                        ::core::result::Result::Ok(_) => {
                            return ::core::result::Result::Err(
                                #crate_path::PersistenceError::DecodeError {
                                    column: #col_name.to_string(),
                                    message: ::std::string::String::from(
                                        "expected text or bytes JSON value",
                                    ),
                                }
                            );
                        }
                    },
                });
                continue;
            }

            collect_used_type_params(field_ty, &type_param_names, &mut from_db_value_params);
            if attrs.default {
                field_inits.push(quote! {
                    #field_ident: match row.get_opt::<#field_ty>(#col_name) {
                        ::core::result::Result::Ok(::core::option::Option::Some(v)) => v,
                        ::core::result::Result::Ok(::core::option::Option::None)
                        | ::core::result::Result::Err(
                            #crate_path::PersistenceError::ColumnNotFound(_)
                        ) => ::core::default::Default::default(),
                        ::core::result::Result::Err(err) => {
                            return ::core::result::Result::Err(err);
                        }
                    },
                });
            } else {
                field_inits.push(quote! {
                    #field_ident: row.get::<#field_ty>(#col_name)?,
                });
            }
        }
    }

    let mut generics = input.generics.clone();
    if !from_db_value_params.is_empty()
        || !from_row_params.is_empty()
        || !json_params.is_empty()
        || !default_params.is_empty()
    {
        let where_clause = generics.make_where_clause();
        for tp in input.generics.type_params() {
            let ident = &tp.ident;
            let name = ident.to_string();
            if from_db_value_params.contains(&name) {
                where_clause
                    .predicates
                    .push(parse_quote!(#ident: #crate_path::FromDbValue));
            }
            if from_row_params.contains(&name) {
                where_clause
                    .predicates
                    .push(parse_quote!(#ident: #crate_path::FromRow));
            }
            if json_params.contains(&name) {
                where_clause
                    .predicates
                    .push(parse_quote!(#ident: ::serde::de::DeserializeOwned));
            }
            if default_params.contains(&name) {
                where_clause
                    .predicates
                    .push(parse_quote!(#ident: ::core::default::Default));
            }
        }
    }

    let struct_ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let construct = if named_fields.is_some() {
        quote! {
            Self {
                #(#field_inits)*
            }
        }
    } else {
        quote! { Self }
    };

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #crate_path::FromRow for #struct_ident #ty_generics #where_clause {
            fn from_row(row: &#crate_path::DbRow) -> ::core::result::Result<Self, #crate_path::PersistenceError> {
                ::core::result::Result::Ok(#construct)
            }
        }
    })
}

/// Parses `#[from_row(crate = "...")]` on the container, defaulting to `::rume::persistence`.
fn parse_from_row_crate_path(attrs: &[Attribute]) -> syn::Result<Path> {
    let mut crate_path: Path = parse_quote!(::rume::persistence);

    for attr in attrs {
        if !attr.path().is_ident("from_row") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let lit: LitStr = meta.value()?.parse()?;
                crate_path = lit.parse::<Path>()?;
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    meta.path,
                    "unknown #[from_row(...)] container attribute; expected `crate = \"...\"`",
                ))
            }
        })?;
    }

    Ok(crate_path)
}

fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut parsed = FieldAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("from_row") {
            continue;
        }
        parsed
            .attr_span
            .get_or_insert_with(|| attr.pound_token.span);

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let lit: LitStr = meta.value()?.parse()?;
                parsed.rename = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("default") {
                parsed.default = true;
                Ok(())
            } else if meta.path.is_ident("skip") {
                parsed.skip = true;
                Ok(())
            } else if meta.path.is_ident("flatten") {
                parsed.flatten = true;
                Ok(())
            } else if meta.path.is_ident("json") {
                parsed.json = true;
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    meta.path,
                    "unknown #[from_row(...)] field attribute; expected `rename = \"...\"`, `default`, `skip`, `flatten`, or `json`",
                ))
            }
        })?;
    }

    let span = parsed.attr_span.unwrap_or_else(Span::call_site);
    if parsed.skip && (parsed.rename.is_some() || parsed.flatten || parsed.json) {
        return Err(syn::Error::new(
            span,
            "#[from_row(skip)] cannot be combined with `rename`, `flatten`, or `json`",
        ));
    }
    if parsed.flatten && (parsed.rename.is_some() || parsed.json || parsed.default) {
        return Err(syn::Error::new(
            span,
            "#[from_row(flatten)] cannot be combined with `rename`, `json`, or `default`",
        ));
    }

    Ok(parsed)
}
