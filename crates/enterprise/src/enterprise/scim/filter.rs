//! RFC 7644 §3.4.2.2 SCIM 2.0 filter AST, bounded recursive-descent parser, and evaluator.

use thiserror::Error;

use super::{ScimErrorResponse, ScimGroup, ScimUser};

/// Default maximum filter string length in bytes to bound parser memory/CPU.
pub const DEFAULT_MAX_FILTER_LEN: usize = 4096;
/// Default maximum nesting depth for parentheses / `not` / boolean expressions.
pub const DEFAULT_MAX_FILTER_DEPTH: usize = 32;

/// Comparison operators defined by RFC 7644 §3.4.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScimFilterOp {
    Eq,
    Ne,
    Co,
    Sw,
    Ew,
    Gt,
    Ge,
    Lt,
    Le,
}

/// Literal comparison values in a SCIM filter expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ScimFilterValue {
    String(String),
    Bool(bool),
    Number(f64),
    Null,
}

/// Parsed RFC 7644 SCIM filter AST.
#[derive(Debug, Clone, PartialEq)]
pub enum ScimFilter {
    And(Box<ScimFilter>, Box<ScimFilter>),
    Or(Box<ScimFilter>, Box<ScimFilter>),
    Not(Box<ScimFilter>),
    Presence {
        attr: String,
    },
    Compare {
        attr: String,
        op: ScimFilterOp,
        value: ScimFilterValue,
    },
}

/// Errors produced when parsing a SCIM filter string.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScimFilterError {
    #[error("filter length {len} exceeds maximum {max_len}")]
    FilterTooLong { len: usize, max_len: usize },
    #[error("filter nesting depth exceeds maximum {max_depth}")]
    MaxDepthExceeded { max_depth: usize },
    #[error("invalid SCIM filter syntax: {0}")]
    InvalidSyntax(String),
}

impl From<&ScimFilterError> for ScimErrorResponse {
    fn from(err: &ScimFilterError) -> Self {
        ScimErrorResponse::new(400, Some("invalidFilter"), Some(&err.to_string()))
    }
}

impl From<ScimFilterError> for ScimErrorResponse {
    fn from(err: ScimFilterError) -> Self {
        Self::from(&err)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    Word(String),
    StringLit(String),
}

fn tokenize(input: &str) -> Result<Vec<Token>, ScimFilterError> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some(&(_, ch)) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        match ch {
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                let mut closed = false;
                while let Some((_, c)) = chars.next() {
                    if c == '"' {
                        closed = true;
                        break;
                    }
                    if c == '\\' {
                        let (_, esc) = chars.next().ok_or_else(|| {
                            ScimFilterError::InvalidSyntax("unterminated escape sequence".into())
                        })?;
                        match esc {
                            '"' | '\\' | '/' => s.push(esc),
                            'n' => s.push('\n'),
                            'r' => s.push('\r'),
                            't' => s.push('\t'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        }
                    } else {
                        s.push(c);
                    }
                }
                if !closed {
                    return Err(ScimFilterError::InvalidSyntax(
                        "unterminated string literal".into(),
                    ));
                }
                tokens.push(Token::StringLit(s));
            }
            _ => {
                let mut word = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c.is_whitespace() || c == '(' || c == ')' || c == '"' {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }

    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    max_depth: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>, max_depth: usize) -> Self {
        Self {
            tokens,
            pos: 0,
            max_depth,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_word_eq(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(w)) if w.eq_ignore_ascii_case(kw))
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn parse_or(&mut self, depth: usize) -> Result<ScimFilter, ScimFilterError> {
        if depth > self.max_depth {
            return Err(ScimFilterError::MaxDepthExceeded {
                max_depth: self.max_depth,
            });
        }
        let mut left = self.parse_and(depth)?;
        let mut current_depth = depth;
        while self.peek_word_eq("or") {
            // Why: each chained binary `or` adds a level of AST nesting; enforce
            // `max_depth` in the loop so flat chains cannot bypass the depth bound (`RC-21`).
            current_depth += 1;
            if current_depth > self.max_depth {
                return Err(ScimFilterError::MaxDepthExceeded {
                    max_depth: self.max_depth,
                });
            }
            self.next();
            let right = self.parse_and(current_depth)?;
            left = ScimFilter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self, depth: usize) -> Result<ScimFilter, ScimFilterError> {
        if depth > self.max_depth {
            return Err(ScimFilterError::MaxDepthExceeded {
                max_depth: self.max_depth,
            });
        }
        let mut left = self.parse_not(depth)?;
        let mut current_depth = depth;
        while self.peek_word_eq("and") {
            // Why: each chained binary `and` adds a level of AST nesting; enforce
            // `max_depth` in the loop so flat chains cannot bypass the depth bound (`RC-21`).
            current_depth += 1;
            if current_depth > self.max_depth {
                return Err(ScimFilterError::MaxDepthExceeded {
                    max_depth: self.max_depth,
                });
            }
            self.next();
            let right = self.parse_not(current_depth)?;
            left = ScimFilter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self, depth: usize) -> Result<ScimFilter, ScimFilterError> {
        if depth > self.max_depth {
            return Err(ScimFilterError::MaxDepthExceeded {
                max_depth: self.max_depth,
            });
        }
        if self.peek_word_eq("not") {
            self.next();
            let inner = self.parse_not(depth + 1)?;
            return Ok(ScimFilter::Not(Box::new(inner)));
        }
        self.parse_primary(depth)
    }

    fn parse_primary(&mut self, depth: usize) -> Result<ScimFilter, ScimFilterError> {
        if depth > self.max_depth {
            return Err(ScimFilterError::MaxDepthExceeded {
                max_depth: self.max_depth,
            });
        }
        match self.peek() {
            Some(Token::LParen) => {
                self.next();
                let expr = self.parse_or(depth + 1)?;
                match self.next() {
                    Some(Token::RParen) => Ok(expr),
                    _ => Err(ScimFilterError::InvalidSyntax(
                        "expected closing ')'".into(),
                    )),
                }
            }
            Some(Token::Word(_)) => self.parse_attr_expr(),
            other => Err(ScimFilterError::InvalidSyntax(format!(
                "unexpected token: {other:?}"
            ))),
        }
    }

    fn parse_attr_expr(&mut self) -> Result<ScimFilter, ScimFilterError> {
        let attr = match self.next() {
            Some(Token::Word(w)) => w,
            _ => {
                return Err(ScimFilterError::InvalidSyntax(
                    "expected attribute name".into(),
                ));
            }
        };

        let op_word = match self.next() {
            Some(Token::Word(w)) => w,
            other => {
                return Err(ScimFilterError::InvalidSyntax(format!(
                    "expected operator after '{attr}', found {other:?}"
                )));
            }
        };

        if op_word.eq_ignore_ascii_case("pr") {
            return Ok(ScimFilter::Presence { attr });
        }

        let op = match op_word.to_ascii_lowercase().as_str() {
            "eq" => ScimFilterOp::Eq,
            "ne" => ScimFilterOp::Ne,
            "co" => ScimFilterOp::Co,
            "sw" => ScimFilterOp::Sw,
            "ew" => ScimFilterOp::Ew,
            "gt" => ScimFilterOp::Gt,
            "ge" => ScimFilterOp::Ge,
            "lt" => ScimFilterOp::Lt,
            "le" => ScimFilterOp::Le,
            other => {
                return Err(ScimFilterError::InvalidSyntax(format!(
                    "unknown SCIM operator '{other}'"
                )));
            }
        };

        let value = match self.next() {
            Some(Token::StringLit(s)) => ScimFilterValue::String(s),
            Some(Token::Word(w)) => {
                if w.eq_ignore_ascii_case("true") {
                    ScimFilterValue::Bool(true)
                } else if w.eq_ignore_ascii_case("false") {
                    ScimFilterValue::Bool(false)
                } else if w.eq_ignore_ascii_case("null") {
                    ScimFilterValue::Null
                } else if let Ok(n) = w.parse::<f64>() {
                    ScimFilterValue::Number(n)
                } else {
                    return Err(ScimFilterError::InvalidSyntax(format!(
                        "invalid filter value '{w}' (strings must be double-quoted)"
                    )));
                }
            }
            other => {
                return Err(ScimFilterError::InvalidSyntax(format!(
                    "expected comparison value, found {other:?}"
                )));
            }
        };

        Ok(ScimFilter::Compare { attr, op, value })
    }
}

impl ScimFilter {
    /// Parses a SCIM filter using [`DEFAULT_MAX_FILTER_LEN`] and [`DEFAULT_MAX_FILTER_DEPTH`].
    pub fn parse(input: &str) -> Result<Self, ScimFilterError> {
        Self::parse_with_limits(input, DEFAULT_MAX_FILTER_LEN, DEFAULT_MAX_FILTER_DEPTH)
    }

    /// Parses a SCIM filter with explicit length and nesting-depth limits.
    pub fn parse_with_limits(
        input: &str,
        max_len: usize,
        max_depth: usize,
    ) -> Result<Self, ScimFilterError> {
        if input.len() > max_len {
            return Err(ScimFilterError::FilterTooLong {
                len: input.len(),
                max_len,
            });
        }
        let tokens = tokenize(input)?;
        if tokens.is_empty() {
            return Err(ScimFilterError::InvalidSyntax(
                "filter cannot be empty".into(),
            ));
        }
        let mut parser = Parser::new(tokens, max_depth);
        let ast = parser.parse_or(0)?;
        if parser.pos < parser.tokens.len() {
            return Err(ScimFilterError::InvalidSyntax(format!(
                "unexpected trailing token {:?}",
                parser.tokens[parser.pos]
            )));
        }
        Ok(ast)
    }

    /// Evaluates this filter against a [`ScimUser`].
    pub fn matches_user(&self, user: &ScimUser) -> bool {
        self.eval(&|attr| resolve_user_attr(user, attr))
    }

    /// Evaluates this filter against a [`ScimGroup`].
    pub fn matches_group(&self, group: &ScimGroup) -> bool {
        self.eval(&|attr| resolve_group_attr(group, attr))
    }

    fn eval<'a, F>(&self, resolve: &F) -> bool
    where
        F: Fn(&str) -> Vec<AttrVal<'a>>,
    {
        match self {
            Self::And(a, b) => a.eval(resolve) && b.eval(resolve),
            Self::Or(a, b) => a.eval(resolve) || b.eval(resolve),
            Self::Not(inner) => !inner.eval(resolve),
            Self::Presence { attr } => resolve(attr).iter().any(AttrVal::is_present),
            Self::Compare { attr, op, value } => {
                let vals = resolve(attr);
                match (op, value) {
                    (ScimFilterOp::Eq, ScimFilterValue::Null) => {
                        !vals.iter().any(AttrVal::is_present)
                    }
                    (ScimFilterOp::Ne, ScimFilterValue::Null) => {
                        vals.iter().any(AttrVal::is_present)
                    }
                    (_, ScimFilterValue::Null) => false,
                    // Why: per RFC 7644 §3.4.2.2, `ne` on multi-valued or absent
                    // attributes is the negation of `eq` across the value set:
                    // `["a", "b"] ne "a"` is `false`, and absent `ne "a"` is `true`.
                    (ScimFilterOp::Ne, _) => !vals
                        .iter()
                        .any(|v| compare_attr(v, ScimFilterOp::Eq, value)),
                    _ => vals.iter().any(|v| compare_attr(v, *op, value)),
                }
            }
        }
    }
}

enum AttrVal<'a> {
    Str(&'a str),
    Bool(bool),
    Present,
}

impl AttrVal<'_> {
    fn is_present(&self) -> bool {
        match self {
            Self::Str(s) => !s.is_empty(),
            Self::Bool(_) | Self::Present => true,
        }
    }
}

fn push_opt_str<'a>(out: &mut Vec<AttrVal<'a>>, opt: Option<&'a str>) {
    if let Some(s) = opt {
        out.push(AttrVal::Str(s));
    }
}

fn resolve_meta_attr<'a>(meta: Option<&'a super::ScimMeta>, sub: &str) -> Vec<AttrVal<'a>> {
    let mut out = Vec::new();
    let Some(m) = meta else {
        return out;
    };
    match sub {
        "resourcetype" => out.push(AttrVal::Str(&m.resource_type)),
        "created" => push_opt_str(&mut out, m.created.as_deref()),
        "lastmodified" => push_opt_str(&mut out, m.last_modified.as_deref()),
        "location" => push_opt_str(&mut out, m.location.as_deref()),
        "version" => push_opt_str(&mut out, m.version.as_deref()),
        _ => {}
    }
    out
}

fn resolve_user_attr<'a>(user: &'a ScimUser, attr: &str) -> Vec<AttrVal<'a>> {
    let lower = attr.to_ascii_lowercase();
    let mut out = Vec::new();
    match lower.as_str() {
        "id" => out.push(AttrVal::Str(&user.id)),
        "externalid" => push_opt_str(&mut out, user.external_id.as_deref()),
        "username" => out.push(AttrVal::Str(&user.user_name)),
        "displayname" => push_opt_str(&mut out, user.display_name.as_deref()),
        "active" => out.push(AttrVal::Bool(user.active)),
        "name" => {
            if user.name.is_some() {
                out.push(AttrVal::Present);
            }
        }
        "name.formatted" => push_opt_str(
            &mut out,
            user.name.as_ref().and_then(|n| n.formatted.as_deref()),
        ),
        "name.familyname" => push_opt_str(
            &mut out,
            user.name.as_ref().and_then(|n| n.family_name.as_deref()),
        ),
        "name.givenname" => push_opt_str(
            &mut out,
            user.name.as_ref().and_then(|n| n.given_name.as_deref()),
        ),
        "emails" => {
            if !user.emails.is_empty() {
                out.push(AttrVal::Present);
            }
        }
        "emails.value" => {
            for e in &user.emails {
                out.push(AttrVal::Str(&e.value));
            }
        }
        "emails.primary" => {
            for e in &user.emails {
                out.push(AttrVal::Bool(e.primary));
            }
        }
        "emails.type" => {
            for e in &user.emails {
                push_opt_str(&mut out, e.email_type.as_deref());
            }
        }
        "roles" => {
            if !user.roles.is_empty() {
                out.push(AttrVal::Present);
            }
        }
        "roles.value" => {
            for r in &user.roles {
                push_opt_str(&mut out, r.value.as_deref());
            }
        }
        "roles.display" => {
            for r in &user.roles {
                push_opt_str(&mut out, r.display.as_deref());
            }
        }
        "roles.type" => {
            for r in &user.roles {
                push_opt_str(&mut out, r.role_type.as_deref());
            }
        }
        "roles.primary" => {
            for r in &user.roles {
                out.push(AttrVal::Bool(r.primary));
            }
        }
        _ if lower.starts_with("meta.") => {
            return resolve_meta_attr(user.meta.as_ref(), &lower["meta.".len()..]);
        }
        _ => {}
    }
    out
}

fn resolve_group_attr<'a>(group: &'a ScimGroup, attr: &str) -> Vec<AttrVal<'a>> {
    let lower = attr.to_ascii_lowercase();
    let mut out = Vec::new();
    match lower.as_str() {
        "id" => out.push(AttrVal::Str(&group.id)),
        "displayname" => out.push(AttrVal::Str(&group.display_name)),
        "members" => {
            if !group.members.is_empty() {
                out.push(AttrVal::Present);
            }
        }
        "members.value" => {
            for m in &group.members {
                out.push(AttrVal::Str(&m.value));
            }
        }
        "members.$ref" => {
            for m in &group.members {
                push_opt_str(&mut out, m.reference.as_deref());
            }
        }
        "members.display" => {
            for m in &group.members {
                push_opt_str(&mut out, m.display.as_deref());
            }
        }
        _ if lower.starts_with("meta.") => {
            return resolve_meta_attr(group.meta.as_ref(), &lower["meta.".len()..]);
        }
        _ => {}
    }
    out
}

fn compare_attr(attr: &AttrVal<'_>, op: ScimFilterOp, value: &ScimFilterValue) -> bool {
    match (attr, value) {
        (AttrVal::Str(a), ScimFilterValue::String(b)) => {
            let a_low = a.to_ascii_lowercase();
            let b_low = b.to_ascii_lowercase();
            match op {
                ScimFilterOp::Eq => a_low == b_low,
                ScimFilterOp::Ne => a_low != b_low,
                ScimFilterOp::Co => a_low.contains(&b_low),
                ScimFilterOp::Sw => a_low.starts_with(&b_low),
                ScimFilterOp::Ew => a_low.ends_with(&b_low),
                ScimFilterOp::Gt => a_low > b_low,
                ScimFilterOp::Ge => a_low >= b_low,
                ScimFilterOp::Lt => a_low < b_low,
                ScimFilterOp::Le => a_low <= b_low,
            }
        }
        (AttrVal::Bool(a), ScimFilterValue::Bool(b)) => match op {
            ScimFilterOp::Eq => a == b,
            ScimFilterOp::Ne => a != b,
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scim_ne_evaluates_multi_valued_and_absent_attributes_per_rfc7644() {
        let user = ScimUser::new("u1", "alice")
            .with_email("a@example.com", true)
            .with_email("b@example.com", false);

        // Multi-valued `ne`: false if ANY value matches `eq`.
        let ne_present = ScimFilter::parse(r#"emails.value ne "a@example.com""#).unwrap();
        assert!(
            !ne_present.matches_user(&user),
            "multi-valued attribute containing 'a@example.com' must not match `ne \"a@example.com\"`"
        );

        let ne_other = ScimFilter::parse(r#"emails.value ne "c@example.com""#).unwrap();
        assert!(
            ne_other.matches_user(&user),
            "multi-valued attribute not containing 'c@example.com' must match `ne \"c@example.com\"`"
        );

        // Absent attribute `ne`: true per RFC 7644 §3.4.2.2.
        let ne_absent = ScimFilter::parse(r#"displayName ne "Alice""#).unwrap();
        assert!(
            ne_absent.matches_user(&user),
            "absent displayName must match `displayName ne \"Alice\"`"
        );
    }

    #[test]
    fn scim_null_comparisons_handle_absent_and_present_attributes() {
        let mut user = ScimUser::new("u2", "bob");
        let eq_null = ScimFilter::parse("displayName eq null").unwrap();
        let ne_null = ScimFilter::parse("displayName ne null").unwrap();

        assert!(
            eq_null.matches_user(&user),
            "absent displayName must match `eq null`"
        );
        assert!(
            !ne_null.matches_user(&user),
            "absent displayName must not match `ne null`"
        );

        user.display_name = Some("Bob".to_string());
        assert!(
            !eq_null.matches_user(&user),
            "present displayName must not match `eq null`"
        );
        assert!(
            ne_null.matches_user(&user),
            "present displayName must match `ne null`"
        );

        let emails_eq_null = ScimFilter::parse("emails eq null").unwrap();
        let emails_ne_null = ScimFilter::parse("emails ne null").unwrap();
        assert!(emails_eq_null.matches_user(&user));
        assert!(!emails_ne_null.matches_user(&user));

        let user_with_email = user.with_email("bob@example.com", true);
        assert!(!emails_eq_null.matches_user(&user_with_email));
        assert!(emails_ne_null.matches_user(&user_with_email));
    }

    #[test]
    fn scim_chained_or_and_enforce_max_depth() {
        let deep_or = format!("{}a pr", "a pr or ".repeat(40));
        assert_eq!(
            ScimFilter::parse(&deep_or),
            Err(ScimFilterError::MaxDepthExceeded { max_depth: 32 })
        );

        let deep_and = format!("{}a pr", "a pr and ".repeat(40));
        assert_eq!(
            ScimFilter::parse(&deep_and),
            Err(ScimFilterError::MaxDepthExceeded { max_depth: 32 })
        );
    }
}
