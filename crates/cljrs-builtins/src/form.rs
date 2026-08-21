// ── form_to_value ─────────────────────────────────────────────────────────────

use cljrs_gc::GcPtr;
use cljrs_reader::{Form, FormKind};
use cljrs_value::value::SetValue;
use cljrs_value::{
    Keyword, MapValue, PersistentHashSet, PersistentList, PersistentVector, Symbol, Value,
};
use regex::Regex;

// ── anon fn expansion ─────────────────────────────────────────────────────────

/// Expand `#(...)` to `(fn* [p__1 p__2 ... & rest__] ...)`.
pub fn expand_anon_fn(body: &[Form], span: cljrs_types::span::Span) -> Form {
    let mut max_pos: usize = 0;
    let mut has_rest = false;
    find_pct_refs(body, &mut max_pos, &mut has_rest);

    let s = &span;
    let mut params: Vec<Form> = (1..=max_pos)
        .map(|i| Form::new(FormKind::Symbol(format!("p__{i}")), s.clone()))
        .collect();
    if has_rest {
        params.push(Form::new(FormKind::Symbol("&".into()), s.clone()));
        params.push(Form::new(FormKind::Symbol("rest__".into()), s.clone()));
    }

    let new_body = rewrite_pct_refs(body, s.clone());

    // Wrap the rewritten body forms back into a single call expression.
    // #(f a b) → (fn* [params] (f a b)), not (fn* [params] f a b).
    let body_expr = Form::new(FormKind::List(new_body), s.clone());

    Form::new(
        FormKind::List(vec![
            Form::new(FormKind::Symbol("fn*".into()), s.clone()),
            Form::new(FormKind::Vector(params), s.clone()),
            body_expr,
        ]),
        span,
    )
}

fn find_pct_refs(forms: &[Form], max_pos: &mut usize, has_rest: &mut bool) {
    for form in forms {
        find_pct_refs_form(form, max_pos, has_rest);
    }
}

fn find_pct_refs_form(form: &Form, max_pos: &mut usize, has_rest: &mut bool) {
    match &form.kind {
        FormKind::Symbol(s) if (s == "%" || s == "%1") && *max_pos < 1 => {
            *max_pos = 1;
        }
        FormKind::Symbol(s) if s == "%&" => {
            *has_rest = true;
        }
        FormKind::Symbol(s) if s.starts_with('%') => {
            if let Ok(n) = s[1..].parse::<usize>()
                && n > *max_pos
            {
                *max_pos = n;
            }
        }
        FormKind::List(c) | FormKind::Vector(c) | FormKind::Set(c) | FormKind::Map(c) => {
            find_pct_refs(c, max_pos, has_rest);
        }
        // Reader-macro sugar (`@%`, `#'%`, `'%`, `` `% ``, `~%`, `~@%`, `#tag %`)
        // wraps a single inner form; it must be scanned the same as any
        // other nested form so `%` refs under sugar aren't missed.
        FormKind::Quote(inner)
        | FormKind::SyntaxQuote(inner)
        | FormKind::Unquote(inner)
        | FormKind::UnquoteSplice(inner)
        | FormKind::Deref(inner)
        | FormKind::Var(inner)
        | FormKind::TaggedLiteral(_, inner) => {
            find_pct_refs_form(inner, max_pos, has_rest);
        }
        FormKind::Meta(meta, inner) => {
            find_pct_refs_form(meta, max_pos, has_rest);
            find_pct_refs_form(inner, max_pos, has_rest);
        }
        FormKind::ReaderCond { clauses, .. } => {
            find_pct_refs(clauses, max_pos, has_rest);
        }
        _ => {}
    }
}

fn rewrite_pct_refs(forms: &[Form], span: cljrs_types::span::Span) -> Vec<Form> {
    forms
        .iter()
        .map(|f| rewrite_pct_form(f, span.clone()))
        .collect()
}

fn rewrite_pct_form(form: &Form, span: cljrs_types::span::Span) -> Form {
    match &form.kind {
        FormKind::Symbol(s) if s == "%" || s == "%1" => {
            Form::new(FormKind::Symbol("p__1".into()), span)
        }
        FormKind::Symbol(s) if s == "%&" => Form::new(FormKind::Symbol("rest__".into()), span),
        FormKind::Symbol(s) if s.starts_with('%') => {
            if let Ok(n) = s[1..].parse::<usize>() {
                Form::new(FormKind::Symbol(format!("p__{n}")), span)
            } else {
                form.clone()
            }
        }
        FormKind::List(c) => {
            let rewritten = rewrite_pct_refs(c, span.clone());
            Form::new(FormKind::List(rewritten), span)
        }
        FormKind::Vector(c) => {
            let rewritten = rewrite_pct_refs(c, span.clone());
            Form::new(FormKind::Vector(rewritten), span)
        }
        FormKind::Set(c) => {
            let rewritten = rewrite_pct_refs(c, span.clone());
            Form::new(FormKind::Set(rewritten), span)
        }
        FormKind::Map(c) => {
            let rewritten = rewrite_pct_refs(c, span.clone());
            Form::new(FormKind::Map(rewritten), span)
        }
        FormKind::Quote(inner) => Form::new(
            FormKind::Quote(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::SyntaxQuote(inner) => Form::new(
            FormKind::SyntaxQuote(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::Unquote(inner) => Form::new(
            FormKind::Unquote(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::UnquoteSplice(inner) => Form::new(
            FormKind::UnquoteSplice(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::Deref(inner) => Form::new(
            FormKind::Deref(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::Var(inner) => Form::new(
            FormKind::Var(Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::TaggedLiteral(tag, inner) => Form::new(
            FormKind::TaggedLiteral(tag.clone(), Box::new(rewrite_pct_form(inner, span.clone()))),
            span,
        ),
        FormKind::Meta(meta, inner) => Form::new(
            FormKind::Meta(
                Box::new(rewrite_pct_form(meta, span.clone())),
                Box::new(rewrite_pct_form(inner, span.clone())),
            ),
            span,
        ),
        FormKind::ReaderCond { splicing, clauses } => Form::new(
            FormKind::ReaderCond {
                splicing: *splicing,
                clauses: rewrite_pct_refs(clauses, span.clone()),
            },
            span,
        ),
        _ => form.clone(),
    }
}

/// Convert a `Form` to its literal `Value` without evaluating.
/// Used by `quote` and macro expansion.
pub fn form_to_value(form: &Form) -> Value {
    match &form.kind {
        FormKind::Nil => Value::Nil,
        FormKind::Bool(b) => Value::Bool(*b),
        FormKind::Int(n) => Value::Long(*n),
        FormKind::Float(f) => Value::Double(*f),
        FormKind::Symbolic(f) => Value::Double(*f),
        FormKind::Str(s) => Value::string(s.clone()),
        FormKind::Char(c) => Value::Char(*c),
        FormKind::BigInt(s) => crate::parse_bigint(s).unwrap_or(Value::Nil),
        FormKind::BigDecimal(s) => crate::parse_bigdecimal(s).unwrap_or(Value::Nil),
        FormKind::Ratio(s) => crate::parse_ratio(s).unwrap_or(Value::Nil),

        FormKind::Symbol(s) => Value::symbol(Symbol::parse(s)),
        FormKind::Keyword(s) => Value::keyword(Keyword::parse(s)),
        FormKind::AutoKeyword(s) => Value::keyword(Keyword::simple(s.as_str())),
        FormKind::Regex(s) => match Regex::new(s.as_str()) {
            Ok(pattern) => Value::Pattern(GcPtr::new(pattern)),
            Err(_) => Value::Nil, // should already have been caught
        },

        FormKind::List(forms) => {
            let expanded = expand_reader_conds(forms);
            let items: Vec<Value> = expanded.iter().map(form_to_value).collect();
            Value::List(GcPtr::new(PersistentList::from_iter(items)))
        }
        FormKind::Vector(forms) => {
            let expanded = expand_reader_conds(forms);
            let items: Vec<Value> = expanded.iter().map(form_to_value).collect();
            Value::Vector(GcPtr::new(PersistentVector::from_iter(items)))
        }
        FormKind::Map(forms) => {
            let mut m = MapValue::empty();
            for pair in forms.chunks(2) {
                if pair.len() == 2 {
                    m = m.assoc(form_to_value(&pair[0]), form_to_value(&pair[1]));
                }
            }
            Value::Map(m)
        }
        FormKind::Set(forms) => {
            let s = forms
                .iter()
                .fold(PersistentHashSet::empty(), |s, f| s.conj(form_to_value(f)));
            Value::Set(SetValue::Hash(GcPtr::new(s)))
        }

        FormKind::Quote(inner) => {
            // `'x` → the form x as a data value.
            Value::List(GcPtr::new(PersistentList::from_iter([
                Value::symbol(Symbol::simple("quote")),
                form_to_value(inner),
            ])))
        }
        FormKind::SyntaxQuote(inner) => Value::List(GcPtr::new(PersistentList::from_iter([
            Value::symbol(Symbol::simple("syntax-quote")),
            form_to_value(inner),
        ]))),
        FormKind::Unquote(inner) => Value::List(GcPtr::new(PersistentList::from_iter([
            Value::symbol(Symbol::simple("unquote")),
            form_to_value(inner),
        ]))),
        FormKind::UnquoteSplice(inner) => Value::List(GcPtr::new(PersistentList::from_iter([
            Value::symbol(Symbol::simple("unquote-splicing")),
            form_to_value(inner),
        ]))),
        FormKind::Deref(inner) => Value::List(GcPtr::new(PersistentList::from_iter([
            Value::symbol(Symbol::simple("deref")),
            form_to_value(inner),
        ]))),
        FormKind::Var(inner) => Value::List(GcPtr::new(PersistentList::from_iter([
            Value::symbol(Symbol::simple("var")),
            form_to_value(inner),
        ]))),
        FormKind::Meta(_meta, inner) => form_to_value(inner),
        FormKind::AnonFn(body) => {
            // Expand #(...) to (fn* [...] ...) so it round-trips correctly through quote.
            let expanded = expand_anon_fn(body, form.span.clone());
            form_to_value(&expanded)
        }
        FormKind::TaggedLiteral(tag, inner) => match tag.as_str() {
            "uuid" => {
                if let FormKind::Str(s) = &inner.kind {
                    match uuid::Uuid::parse_str(s) {
                        Ok(u) => Value::Uuid(u.as_u128()),
                        Err(_) => form_to_value(inner),
                    }
                } else {
                    form_to_value(inner)
                }
            }
            _ => form_to_value(inner),
        },
        FormKind::ReaderCond {
            splicing: false,
            clauses,
        } => select_reader_cond(clauses).map_or(Value::Nil, form_to_value),
        FormKind::ReaderCond { splicing: true, .. } => Value::Nil, // splice must be handled by parent
    }
}

/// Reader-conditional feature keys for this process. Unset means `["rust"]`.
static READER_FEATURES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Set the feature keys `#?(...)` conditionals match against (e.g.
/// `["bb", "cljrsh", "rust"]` for a babashka-compatible scripting host).
/// `:default` always matches and need not be listed. Call once at startup,
/// before any source is read; returns `false` if the set was already fixed.
pub fn set_reader_features<I, S>(features: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    READER_FEATURES
        .set(features.into_iter().map(Into::into).collect())
        .is_ok()
}

fn feature_matches(key: &str) -> bool {
    match READER_FEATURES.get() {
        Some(features) => features.iter().any(|f| f == key),
        None => key == "rust",
    }
}

/// Resolve a `#?(...)` reader conditional to the selected branch form, or
/// `None` if no clause key is in the process feature set (see
/// [`set_reader_features`]; default `:rust`) and no `:default` clause is
/// present. Clauses are tried in order, matching Clojure: an earlier
/// `:default` shadows a later platform clause.
pub fn select_reader_cond(clauses: &[Form]) -> Option<&Form> {
    let mut i = 0;
    while i + 1 < clauses.len() {
        if let FormKind::Keyword(k) = &clauses[i].kind
            && (k == "default" || feature_matches(k))
        {
            return Some(&clauses[i + 1]);
        }
        i += 2;
    }
    warn_foreign_platform_cond(clauses);
    None
}

/// A conditional with only `:clj`/`:cljs` branches silently expands to
/// nothing on this platform — the most common porting surprise, so say so
/// once per source location.
fn warn_foreign_platform_cond(clauses: &[Form]) {
    let has_foreign = clauses.chunks(2).any(|pair| {
        matches!(&pair[0].kind, FormKind::Keyword(k) if k == "clj" || k == "cljs")
    });
    if !has_foreign {
        return;
    }
    static WARNED: std::sync::Mutex<Option<std::collections::HashSet<(String, u32, u32)>>> =
        std::sync::Mutex::new(None);
    let span = &clauses[0].span;
    let key = (span.file.as_str().to_owned(), span.line, span.col);
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if warned.get_or_insert_with(Default::default).insert(key) {
        eprintln!(
            "WARNING: reader conditional at {}:{}:{} has no matching clause for this \
             platform (only :clj/:cljs); it expands to nothing. Add a :default clause \
             or a platform-specific branch.",
            span.file, span.line, span.col
        );
    }
}

/// Expand reader conditionals in a flat slice of forms.
///
/// - Non-splicing `#?(...)`: replaced by the selected branch (or removed if none).
/// - Splicing `#?@(...)`: selected branch must be a vector/list; its elements
///   are inlined.  If no branch matches, the splice is removed (empty).
pub fn expand_reader_conds(forms: &[Form]) -> Vec<Form> {
    let mut out = Vec::with_capacity(forms.len());
    for form in forms {
        match &form.kind {
            FormKind::ReaderCond {
                splicing: true,
                clauses,
            } => {
                if let Some(selected) = select_reader_cond(clauses) {
                    match &selected.kind {
                        FormKind::Vector(elems) | FormKind::List(elems) => {
                            // Recursively expand any nested reader conditionals
                            // within the spliced elements.
                            let expanded_elems = expand_reader_conds(elems);
                            out.extend(expanded_elems);
                        }
                        // Non-sequence branch: inline it as a single element.
                        _ => out.push(selected.clone()),
                    }
                }
                // No matching branch → splice nothing (empty).
            }
            FormKind::ReaderCond {
                splicing: false,
                clauses,
            } => {
                if let Some(selected) = select_reader_cond(clauses) {
                    out.push(selected.clone());
                }
                // No matching branch → omit.
            }
            _ => out.push(form.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cljrs_reader::Parser;

    fn parse_anon_fn(src: &str) -> Form {
        let mut parser = Parser::new(src.to_string(), "<test>".to_string());
        let form = parser.parse_one().unwrap().unwrap();
        let FormKind::AnonFn(body) = form.kind else {
            panic!("expected AnonFn, got {:?}", form.kind);
        };
        expand_anon_fn(&body, form.span)
    }

    fn arity(expanded: &Form) -> usize {
        let FormKind::List(parts) = &expanded.kind else {
            panic!("expected (fn* [...] ...)");
        };
        let FormKind::Vector(params) = &parts[1].kind else {
            panic!("expected param vector");
        };
        params.len()
    }

    #[test]
    fn deref_sugar_counts_as_one_arg() {
        // #(:x @%) must expand to (fn* [p__1] (:x (deref p__1))), arity 1 —
        // not 0, as if % under `@` were invisible to the arg scanner.
        let expanded = parse_anon_fn("#(:x @%)");
        assert_eq!(arity(&expanded), 1);

        let FormKind::List(parts) = &expanded.kind else {
            unreachable!()
        };
        let FormKind::List(body) = &parts[2].kind else {
            panic!("expected body list");
        };
        let FormKind::Deref(inner) = &body[1].kind else {
            panic!("expected deref form, got {:?}", body[1].kind);
        };
        assert_eq!(inner.kind, FormKind::Symbol("p__1".to_string()));
    }

    fn parse_reader_cond(src: &str) -> Vec<Form> {
        let mut parser = Parser::new(src.to_string(), "<test>".to_string());
        let form = parser.parse_one().unwrap().unwrap();
        let FormKind::ReaderCond { clauses, .. } = form.kind else {
            panic!("expected ReaderCond, got {:?}", form.kind);
        };
        clauses
    }

    #[test]
    fn reader_cond_clauses_tried_in_order() {
        // Clojure semantics: first matching clause wins, and :default matches
        // when reached — an earlier :default shadows a later platform clause.
        let clauses = parse_reader_cond("#?(:default 1 :rust 2)");
        let selected = select_reader_cond(&clauses).expect("expected a match");
        assert_eq!(selected.kind, FormKind::Int(1));
    }

    #[test]
    fn reader_cond_rust_matches() {
        let clauses = parse_reader_cond("#?(:clj 1 :rust 2)");
        let selected = select_reader_cond(&clauses).expect("expected a match");
        assert_eq!(selected.kind, FormKind::Int(2));
    }

    #[test]
    fn reader_cond_foreign_platform_only_is_none() {
        let clauses = parse_reader_cond("#?(:clj 1 :cljs 2)");
        assert!(select_reader_cond(&clauses).is_none());
    }

    #[test]
    fn reader_cond_honors_configured_features() {
        // Keep "rust" in the set so the other tests hold regardless of the
        // order this process-global OnceLock gets initialized in.
        set_reader_features(["bb", "cljrsh", "rust"]);
        let clauses = parse_reader_cond("#?(:bb 1 :rust 2)");
        let selected = select_reader_cond(&clauses).expect("expected a match");
        assert_eq!(selected.kind, FormKind::Int(1));
    }

    #[test]
    fn var_and_meta_sugar_also_scanned() {
        assert_eq!(arity(&parse_anon_fn("#(#'%)")), 1);
        assert_eq!(arity(&parse_anon_fn("#(^:x %)")), 1);
        assert_eq!(arity(&parse_anon_fn("#('%)")), 1);
    }
}
