//! Implementation of `#[derive(HasOpenApiSchema)]`.

use std::collections::BTreeSet;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::meta::ParseNestedMeta;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, ExprUnary, Fields, GenericArgument, Lit, LitStr,
    Path, PathArguments, Token, Type, UnOp, parse_quote,
};

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    match syn::parse::<DeriveInput>(input).and_then(expand_derive) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_derive(input: DeriveInput) -> syn::Result<TokenStream2> {
    let crate_path = parse_openapi_crate_path(&input.attrs)?;
    let container_serde = parse_container_serde(&input.attrs)?;

    let named_fields = match &input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(fields) => Some(&fields.named),
            Fields::Unit => None,
            Fields::Unnamed(fields) => {
                return Err(syn::Error::new_spanned(
                    fields,
                    "HasOpenApiSchema cannot be derived for tuple structs; use named fields instead",
                ));
            }
        },
        Data::Union(data_union) => {
            return Err(syn::Error::new(
                data_union.union_token.span,
                "HasOpenApiSchema cannot be derived for unions; only structs with named fields or unit structs are supported",
            ));
        }
        Data::Enum(data_enum) => {
            return Err(syn::Error::new(
                data_enum.enum_token.span,
                "HasOpenApiSchema cannot be derived for enums; only structs with named fields or unit structs are supported",
            ));
        }
    };

    let type_param_names: BTreeSet<String> = input
        .generics
        .type_params()
        .map(|tp| tp.ident.to_string())
        .collect();
    let mut used_type_params: BTreeSet<String> = BTreeSet::new();
    let mut property_calls = Vec::new();

    if let Some(fields) = named_fields {
        for field in fields {
            let field_serde = parse_field_serde(&field.attrs)?;
            if field_serde.skip {
                continue;
            }

            let Some(field_ident) = &field.ident else {
                continue;
            };
            let raw_name = field_ident.to_string();
            let base_name = raw_name.strip_prefix("r#").unwrap_or(&raw_name);

            let prop_name = match field_serde.rename {
                Some(explicit) => explicit,
                None => match container_serde.rename_all {
                    Some(rule) => rule.apply(base_name),
                    None => base_name.to_string(),
                },
            };

            let field_ty = &field.ty;
            collect_used_type_params(field_ty, &type_param_names, &mut used_type_params);

            let is_optional =
                is_option_type(field_ty) || field_serde.default || container_serde.default;
            let garde_rules = parse_garde_rules(&field.attrs)?;
            let garde_chain = garde_rules.to_builder_calls();

            let schema_expr = quote! {
                <#field_ty as #crate_path::HasOpenApiSchema>::schema()#garde_chain
            };

            if is_optional {
                property_calls.push(quote! {
                    .with_property(#prop_name, #schema_expr)
                });
            } else {
                property_calls.push(quote! {
                    .with_required_property(#prop_name, #schema_expr)
                });
            }
        }
    }

    let mut generics = input.generics.clone();
    if !used_type_params.is_empty() {
        let where_clause = generics.make_where_clause();
        for tp in input.generics.type_params() {
            if used_type_params.contains(&tp.ident.to_string()) {
                let ident = &tp.ident;
                where_clause
                    .predicates
                    .push(parse_quote!(#ident: #crate_path::HasOpenApiSchema));
            }
        }
    }

    let struct_ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #crate_path::HasOpenApiSchema for #struct_ident #ty_generics #where_clause {
            fn schema() -> #crate_path::OpenApiSchema {
                #crate_path::OpenApiSchema::object()
                    #(#property_calls)*
            }
        }
    })
}

/// Parses `#[openapi(crate = "...")]` on the container, defaulting to `::rume::openapi`.
fn parse_openapi_crate_path(attrs: &[Attribute]) -> syn::Result<Path> {
    // Why: default to `::rume::openapi` to match `sql_assets!` and `embed_migrations!`
    // in `crates/macro/src/lib.rs`; callers depending on `rume-http` directly without
    // the `rume` facade override this with `#[openapi(crate = "rume_http::openapi")]`.
    let mut crate_path: Path = parse_quote!(::rume::openapi);

    for attr in attrs {
        if !attr.path().is_ident("openapi") {
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
                    "unknown #[openapi(...)] attribute; expected `crate = \"...\"`",
                ))
            }
        })?;
    }

    Ok(crate_path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl RenameRule {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "lowercase" => Some(Self::Lower),
            "UPPERCASE" => Some(Self::Upper),
            "PascalCase" => Some(Self::Pascal),
            "camelCase" => Some(Self::Camel),
            "snake_case" => Some(Self::Snake),
            "SCREAMING_SNAKE_CASE" => Some(Self::ScreamingSnake),
            "kebab-case" => Some(Self::Kebab),
            "SCREAMING-KEBAB-CASE" => Some(Self::ScreamingKebab),
            _ => None,
        }
    }

    fn apply(self, field: &str) -> String {
        match self {
            Self::Lower => field.to_ascii_lowercase(),
            Self::Upper | Self::ScreamingSnake => field.to_ascii_uppercase(),
            Self::Snake => field.to_string(),
            Self::Kebab => field.replace('_', "-"),
            Self::ScreamingKebab => field.replace('_', "-").to_ascii_uppercase(),
            Self::Pascal => {
                let mut out = String::with_capacity(field.len());
                for segment in field.split('_').filter(|s| !s.is_empty()) {
                    let mut chars = segment.chars();
                    if let Some(first) = chars.next() {
                        out.extend(first.to_uppercase());
                        out.push_str(chars.as_str());
                    }
                }
                out
            }
            Self::Camel => {
                let pascal = Self::Pascal.apply(field);
                let mut chars = pascal.chars();
                match chars.next() {
                    Some(first) => {
                        let mut out = String::with_capacity(pascal.len());
                        out.extend(first.to_lowercase());
                        out.push_str(chars.as_str());
                        out
                    }
                    None => String::new(),
                }
            }
        }
    }
}

#[derive(Default)]
struct ContainerSerdeAttrs {
    rename_all: Option<RenameRule>,
    default: bool,
}

#[derive(Default)]
struct FieldSerdeAttrs {
    rename: Option<String>,
    skip: bool,
    default: bool,
}

fn parse_container_serde(attrs: &[Attribute]) -> syn::Result<ContainerSerdeAttrs> {
    let mut out = ContainerSerdeAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if meta.input.peek(Token![=]) {
                    let lit: LitStr = meta.value()?.parse()?;
                    if let Some(rule) = RenameRule::from_str(&lit.value()) {
                        out.rename_all = Some(rule);
                    }
                } else if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|nested| {
                        if nested.path.is_ident("deserialize") && nested.input.peek(Token![=]) {
                            let lit: LitStr = nested.value()?.parse()?;
                            if let Some(rule) = RenameRule::from_str(&lit.value()) {
                                out.rename_all = Some(rule);
                            }
                            Ok(())
                        } else {
                            skip_meta_value(&nested)
                        }
                    })?;
                }
                Ok(())
            } else if meta.path.is_ident("default") {
                out.default = true;
                skip_meta_value(&meta)
            } else {
                skip_meta_value(&meta)
            }
        })?;
    }

    Ok(out)
}

fn parse_field_serde(attrs: &[Attribute]) -> syn::Result<FieldSerdeAttrs> {
    let mut out = FieldSerdeAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                if meta.input.peek(Token![=]) {
                    let lit: LitStr = meta.value()?.parse()?;
                    out.rename = Some(lit.value());
                } else if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|nested| {
                        if nested.path.is_ident("deserialize") && nested.input.peek(Token![=]) {
                            let lit: LitStr = nested.value()?.parse()?;
                            out.rename = Some(lit.value());
                            Ok(())
                        } else {
                            skip_meta_value(&nested)
                        }
                    })?;
                }
                Ok(())
            } else if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                out.skip = true;
                skip_meta_value(&meta)
            } else if meta.path.is_ident("default") {
                out.default = true;
                skip_meta_value(&meta)
            } else {
                skip_meta_value(&meta)
            }
        })?;
    }

    Ok(out)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct GardeRules {
    format: Option<String>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    minimum: Option<i64>,
    maximum: Option<i64>,
    pattern: Option<String>,
}

impl GardeRules {
    fn to_builder_calls(&self) -> TokenStream2 {
        let mut calls = TokenStream2::new();

        if let Some(format) = &self.format {
            calls.extend(quote! { .with_format(#format) });
        }
        if let Some(min) = self.min_length {
            calls.extend(quote! { .with_min_length(#min) });
        }
        if let Some(max) = self.max_length {
            calls.extend(quote! { .with_max_length(#max) });
        }
        if let Some(min) = self.minimum {
            calls.extend(quote! { .with_minimum(#min) });
        }
        if let Some(max) = self.maximum {
            calls.extend(quote! { .with_maximum(#max) });
        }
        if let Some(pattern) = &self.pattern {
            calls.extend(quote! { .with_pattern(#pattern) });
        }

        calls
    }
}

fn parse_garde_rules(attrs: &[Attribute]) -> syn::Result<GardeRules> {
    let mut rules = GardeRules::default();

    for attr in attrs {
        if !attr.path().is_ident("garde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("email") {
                rules.format = Some("email".to_string());
                skip_meta_value(&meta)
            } else if meta.path.is_ident("url") {
                rules.format = Some("uri".to_string());
                skip_meta_value(&meta)
            } else if meta.path.is_ident("length") && meta.input.peek(syn::token::Paren) {
                parse_garde_length(&meta, &mut rules)
            } else if meta.path.is_ident("range") && meta.input.peek(syn::token::Paren) {
                parse_garde_range(&meta, &mut rules)
            } else if meta.path.is_ident("pattern") && meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                if content.peek(LitStr) {
                    let lit: LitStr = content.parse()?;
                    if content.is_empty() {
                        rules.pattern = Some(lit.value());
                    } else {
                        let _: TokenStream2 = content.parse()?;
                    }
                } else {
                    let _: TokenStream2 = content.parse()?;
                }
                Ok(())
            } else {
                skip_meta_value(&meta)
            }
        })?;
    }

    Ok(rules)
}

fn parse_garde_length(meta: &ParseNestedMeta<'_>, rules: &mut GardeRules) -> syn::Result<()> {
    meta.parse_nested_meta(|nested| {
        if nested.path.is_ident("min") && nested.input.peek(Token![=]) {
            let expr: Expr = nested.value()?.parse()?;
            if let Some(val) = parse_usize_lit(&expr) {
                rules.min_length = Some(val);
            }
            Ok(())
        } else if nested.path.is_ident("max") && nested.input.peek(Token![=]) {
            let expr: Expr = nested.value()?.parse()?;
            if let Some(val) = parse_usize_lit(&expr) {
                rules.max_length = Some(val);
            }
            Ok(())
        } else if nested.path.is_ident("chars") && nested.input.peek(syn::token::Paren) {
            parse_garde_length(&nested, rules)
        } else {
            skip_meta_value(&nested)
        }
    })
}

fn parse_garde_range(meta: &ParseNestedMeta<'_>, rules: &mut GardeRules) -> syn::Result<()> {
    meta.parse_nested_meta(|nested| {
        if nested.path.is_ident("min") && nested.input.peek(Token![=]) {
            let expr: Expr = nested.value()?.parse()?;
            if let Some(val) = parse_i64_lit(&expr) {
                rules.minimum = Some(val);
            }
            Ok(())
        } else if nested.path.is_ident("max") && nested.input.peek(Token![=]) {
            let expr: Expr = nested.value()?.parse()?;
            if let Some(val) = parse_i64_lit(&expr) {
                rules.maximum = Some(val);
            }
            Ok(())
        } else {
            skip_meta_value(&nested)
        }
    })
}

fn skip_meta_value(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        let _: Expr = meta.value()?.parse()?;
    } else if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let _: TokenStream2 = content.parse()?;
    }
    Ok(())
}

fn parse_usize_lit(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Group(group) => parse_usize_lit(&group.expr),
        Expr::Lit(ExprLit {
            lit: Lit::Int(lit_int),
            ..
        }) => lit_int.base10_parse::<usize>().ok(),
        _ => None,
    }
}

fn parse_i64_lit(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Group(group) => parse_i64_lit(&group.expr),
        Expr::Lit(ExprLit {
            lit: Lit::Int(lit_int),
            ..
        }) => lit_int.base10_parse::<i64>().ok(),
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr: inner,
            ..
        }) => match inner.as_ref() {
            Expr::Lit(ExprLit {
                lit: Lit::Int(lit_int),
                ..
            }) => format!("-{}", lit_int.base10_digits()).parse::<i64>().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Returns `true` if `ty` is `Option<T>`, `std::option::Option<T>`, or `core::option::Option<T>`.
fn is_option_type(ty: &Type) -> bool {
    let type_path = match ty {
        Type::Group(group) => return is_option_type(&group.elem),
        Type::Path(type_path) if type_path.qself.is_none() => type_path,
        _ => return false,
    };

    let segments: Vec<&syn::PathSegment> = type_path.path.segments.iter().collect();
    let is_option_path = match segments.as_slice() {
        [only] => only.ident == "Option",
        [root, mid, last] => {
            (root.ident == "std" || root.ident == "core")
                && mid.ident == "option"
                && last.ident == "Option"
        }
        _ => false,
    };

    if !is_option_path {
        return false;
    }

    matches!(
        segments.last().map(|seg| &seg.arguments),
        Some(PathArguments::AngleBracketed(args))
            if args.args.len() == 1 && matches!(args.args.first(), Some(GenericArgument::Type(_)))
    )
}

fn collect_used_type_params(
    ty: &Type,
    type_param_names: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
) {
    match ty {
        Type::Path(type_path) => {
            if let Some(qself) = &type_path.qself {
                collect_used_type_params(&qself.ty, type_param_names, used);
            } else if type_path.path.leading_colon.is_none()
                && let Some(first) = type_path.path.segments.first()
            {
                let name = first.ident.to_string();
                if type_param_names.contains(&name) {
                    used.insert(name);
                }
            }
            for seg in &type_path.path.segments {
                if let PathArguments::AngleBracketed(args) = &seg.arguments {
                    for arg in &args.args {
                        match arg {
                            GenericArgument::Type(inner) => {
                                collect_used_type_params(inner, type_param_names, used);
                            }
                            GenericArgument::AssocType(assoc) => {
                                collect_used_type_params(&assoc.ty, type_param_names, used);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Type::Reference(r) => collect_used_type_params(&r.elem, type_param_names, used),
        Type::Slice(s) => collect_used_type_params(&s.elem, type_param_names, used),
        Type::Array(a) => collect_used_type_params(&a.elem, type_param_names, used),
        Type::Tuple(t) => {
            for elem in &t.elems {
                collect_used_type_params(elem, type_param_names, used);
            }
        }
        Type::Paren(p) => collect_used_type_params(&p.elem, type_param_names, used),
        Type::Group(g) => collect_used_type_params(&g.elem, type_param_names, used),
        Type::Ptr(p) => collect_used_type_params(&p.elem, type_param_names, used),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use syn::{Field, parse_quote};

    use super::{GardeRules, RenameRule, is_option_type, parse_garde_rules};

    #[test]
    fn rename_all_transforms_snake_case_to_all_supported_conventions() {
        assert_eq!(
            RenameRule::from_str("camelCase")
                .unwrap()
                .apply("user_first_name"),
            "userFirstName"
        );
        assert_eq!(
            RenameRule::from_str("PascalCase")
                .unwrap()
                .apply("user_first_name"),
            "UserFirstName"
        );
        assert_eq!(
            RenameRule::from_str("kebab-case")
                .unwrap()
                .apply("user_first_name"),
            "user-first-name"
        );
        assert_eq!(
            RenameRule::from_str("SCREAMING_SNAKE_CASE")
                .unwrap()
                .apply("user_first_name"),
            "USER_FIRST_NAME"
        );
        assert_eq!(
            RenameRule::from_str("snake_case")
                .unwrap()
                .apply("user_first_name"),
            "user_first_name"
        );
        assert_eq!(
            RenameRule::from_str("lowercase")
                .unwrap()
                .apply("User_First_Name"),
            "user_first_name"
        );
        assert_eq!(
            RenameRule::from_str("UPPERCASE")
                .unwrap()
                .apply("user_first_name"),
            "USER_FIRST_NAME"
        );
    }

    #[test]
    fn is_option_type_recognizes_option_variants() {
        let opt_str: syn::Type = parse_quote!(Option<String>);
        let std_opt: syn::Type = parse_quote!(std::option::Option<i32>);
        let std_abs_opt: syn::Type = parse_quote!(::std::option::Option<i32>);
        let core_opt: syn::Type = parse_quote!(core::option::Option<T>);
        let core_abs_opt: syn::Type = parse_quote!(::core::option::Option<T>);
        let vec_opt: syn::Type = parse_quote!(Vec<Option<String>>);
        let my_opt: syn::Type = parse_quote!(MyOption<T>);
        let custom_mod_opt: syn::Type = parse_quote!(my_mod::Option<T>);

        assert!(is_option_type(&opt_str));
        assert!(is_option_type(&std_opt));
        assert!(is_option_type(&std_abs_opt));
        assert!(is_option_type(&core_opt));
        assert!(is_option_type(&core_abs_opt));
        assert!(!is_option_type(&vec_opt));
        assert!(!is_option_type(&my_opt));
        assert!(!is_option_type(&custom_mod_opt));
    }

    #[test]
    fn parse_garde_rules_extracts_email_url_length_range_and_pattern() {
        let email_field: Field = parse_quote! {
            #[garde(email)]
            email: String
        };
        assert_eq!(
            parse_garde_rules(&email_field.attrs).unwrap(),
            GardeRules {
                format: Some("email".to_string()),
                ..Default::default()
            }
        );

        let url_field: Field = parse_quote! {
            #[garde(url)]
            website: String
        };
        assert_eq!(
            parse_garde_rules(&url_field.attrs).unwrap(),
            GardeRules {
                format: Some("uri".to_string()),
                ..Default::default()
            }
        );

        let length_field: Field = parse_quote! {
            #[garde(length(min = 3, max = 50))]
            username: String
        };
        assert_eq!(
            parse_garde_rules(&length_field.attrs).unwrap(),
            GardeRules {
                min_length: Some(3),
                max_length: Some(50),
                ..Default::default()
            }
        );

        let chars_length_field: Field = parse_quote! {
            #[garde(length(chars(min = 2, max = 20)))]
            bio: String
        };
        assert_eq!(
            parse_garde_rules(&chars_length_field.attrs).unwrap(),
            GardeRules {
                min_length: Some(2),
                max_length: Some(20),
                ..Default::default()
            }
        );

        let range_field: Field = parse_quote! {
            #[garde(range(min = 18, max = 120))]
            age: i64
        };
        assert_eq!(
            parse_garde_rules(&range_field.attrs).unwrap(),
            GardeRules {
                minimum: Some(18),
                maximum: Some(120),
                ..Default::default()
            }
        );

        let negative_and_float_range_field: Field = parse_quote! {
            #[garde(range(min = -10, max = 99.5))]
            delta: f64
        };
        assert_eq!(
            parse_garde_rules(&negative_and_float_range_field.attrs).unwrap(),
            GardeRules {
                minimum: Some(-10),
                maximum: None,
                ..Default::default()
            }
        );

        let pattern_field: Field = parse_quote! {
            #[garde(pattern("^[a-z]+$"))]
            slug: String
        };
        assert_eq!(
            parse_garde_rules(&pattern_field.attrs).unwrap(),
            GardeRules {
                pattern: Some("^[a-z]+$".to_string()),
                ..Default::default()
            }
        );

        let ignored_field: Field = parse_quote! {
            #[garde(dive, custom(my_fn), skip)]
            nested: String
        };
        assert_eq!(
            parse_garde_rules(&ignored_field.attrs).unwrap(),
            GardeRules::default()
        );
    }
}
