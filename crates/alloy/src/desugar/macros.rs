//! Macro expansion and the $-prefixed intrinsics.

use std::collections::HashMap;

use alloy_syntax::ast::{Expr, TokSpan};

use super::types::literal_type;
use super::*;

/// A macro body captured as one-line source, so an expansion re-parses.
#[derive(Clone)]
pub(crate) struct MacroRef {
    pub(crate) params: Vec<String>,
    /// The default of each parameter, as source text. A parameter with
    /// one is optional, the way a function's is.
    pub(crate) defaults: Vec<Option<String>>,
    /// What each pattern parameter binds, as `MacroSource::patterns`.
    pub(crate) patterns: Vec<Vec<(String, String)>>,
    pub(crate) variadic: bool,
    /// The statements, tokens joined by spaces.
    pub(crate) body: String,
    /// The trailing expression, tokens joined by spaces.
    pub(crate) tail: Option<String>,
}

/// The tokens of a one-line macro body, each with the gap in front of
/// it. The body keeps the spacing the declaration wrote, so a word is
/// what the lexer says, not what a space separates. A string holds its
/// own spaces, and a path such as `Choice.Yes` holds none.
fn body_parts(body: &str) -> Vec<(&str, &str)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(body) else {
        // A body the lexer refuses still substitutes by whole words.
        return body
            .split(' ')
            .enumerate()
            .map(|(i, w)| (if i == 0 { "" } else { " " }, w))
            .collect();
    };
    let mut out = Vec::with_capacity(lexed.toks.len());
    let mut at = 0usize;

    for tok in &lexed.toks {
        out.push((&body[at..tok.start as usize], tok.text(body)));
        at = tok.end as usize;
    }

    out
}

/// Text that reads the same with or without parentheses around it.
/// The names a macro body declares: after `local`, the variables of a
/// `for`, and the parameters of a `function` inside it. The body is one
/// line of the tokens the declaration wrote.
pub(crate) fn body_locals(body: &str) -> Vec<String> {
    let words: Vec<&str> = body_parts(body).into_iter().map(|(_, w)| w).collect();
    let is_name = |w: &str| {
        w.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && w.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !matches!(w, "function" | "in" | "do" | "end" | "local" | "for")
    };
    let mut out = Vec::new();
    let mut i = 0;

    while i < words.len() {
        match words[i] {
            "local" => {
                let mut j = i + 1;

                if words.get(j) == Some(&"function") {
                    j += 1;
                }

                while let Some(w) = words.get(j) {
                    if is_name(w) {
                        out.push((*w).to_string());
                        j += 1;

                        // A type annotation runs to the next comma or `=`.
                        if words.get(j) == Some(&":") {
                            while let Some(t) = words.get(j)
                                && !matches!(*t, "," | "=")
                                && !(j > i + 1 && is_name(t) && words.get(j - 1) == Some(&","))
                            {
                                j += 1;
                            }
                        }
                    }

                    if words.get(j) == Some(&",") {
                        j += 1;
                    } else {
                        break;
                    }
                }
            }

            "for" => {
                let mut j = i + 1;

                while let Some(w) = words.get(j)
                    && !matches!(*w, "in" | "=" | "do")
                {
                    if is_name(w) {
                        out.push((*w).to_string());
                    }

                    j += 1;
                }
            }

            "function" => {
                let mut j = i + 1;

                while let Some(w) = words.get(j)
                    && *w != "("
                    && !matches!(*w, "end" | "local")
                {
                    j += 1;
                }

                if words.get(j) == Some(&"(") {
                    let mut depth = 0i32;
                    let mut at_start = true;

                    while let Some(w) = words.get(j) {
                        match *w {
                            "(" | "{" | "[" | "<" => depth += 1,
                            ")" | "}" | "]" | ">" => depth -= 1,
                            _ => {}
                        }

                        if depth == 0 {
                            break;
                        }

                        if at_start && is_name(w) && depth == 1 && *w != "self" {
                            out.push((*w).to_string());
                        }

                        at_start = matches!(*w, "(" | ",") && depth == 1;
                        j += 1;
                    }
                }
            }

            _ => {}
        }

        i += 1;
    }

    out
}

/// Where a `return` sits in a one-line macro body.
enum BodyReturn {
    /// The body writes no `return`.
    None,
    /// The body ends in `return <value>`: the byte the `return` starts
    /// at, and the value it carries.
    Tail(usize, String),
    /// Every other shape: an early `return`, one with no value, and one
    /// with more than one. Such a body has no value for an expression.
    /// `last` is the byte the body's trailing `return` starts at, when
    /// the body has one.
    Other { last: Option<usize> },
}

impl BodyReturn {
    /// The byte the body's trailing `return` starts at. Such a body
    /// returns from the function around a call in statement position,
    /// so nothing can follow the call in its block.
    fn last_return_at(&self) -> Option<usize> {
        match self {
            Self::Tail(at, _) | Self::Other { last: Some(at) } => Some(*at),

            _ => None,
        }
    }
}

/// Reads a macro body for the `return` that gives it a value.
///
/// In expression position a macro's value is its last expression, so a
/// body that ends in `return <value>` gives that value and the `return`
/// drops out. The body is one line of the tokens the declaration wrote.
fn body_return(body: &str) -> BodyReturn {
    let returns = body_parts(body)
        .iter()
        .filter(|(_, word)| *word == "return")
        .count();

    if returns == 0 {
        return BodyReturn::None;
    }

    let Ok(lexed) = alloy_syntax::lexer::lex(body) else {
        return BodyReturn::Other { last: None };
    };
    let (chunk, errors) =
        alloy_syntax::parser::parse_lenient(body, &lexed.toks, Default::default());

    if !errors.is_empty() {
        return BodyReturn::Other { last: None };
    }
    let last = chunk.block.stmts.last();

    match last {
        // One `return`, last, with one value: the value of the body.
        Some(alloy_syntax::ast::Stmt::Return(r)) if returns == 1 && r.values.len() == 1 => {
            let at = lexed.toks[r.span.start as usize].start as usize;

            BodyReturn::Tail(at, r.values[0].span().text(body, &lexed.toks).to_string())
        }

        _ => BodyReturn::Other {
            last: match last {
                Some(alloy_syntax::ast::Stmt::Return(r)) => {
                    Some(lexed.toks[r.span.start as usize].start as usize)
                }

                _ => None,
            },
        },
    }
}

/// The one expression a macro body is, when the parser read it as a
/// statement: `print(x)` or `new Pt { x = 0 }`. In expression position
/// that expression is the body's value. A body of any other shape has
/// no value, and the caller reports it.
fn body_value(body: &str) -> Option<String> {
    let lexed = alloy_syntax::lexer::lex(body).ok()?;
    let (chunk, errors) =
        alloy_syntax::parser::parse_lenient(body, &lexed.toks, Default::default());

    if !errors.is_empty() {
        return None;
    }

    match chunk.block.stmts.as_slice() {
        [alloy_syntax::ast::Stmt::Call(e, _)] => Some(e.span().text(body, &lexed.toks).to_string()),

        _ => None,
    }
}

/// The last name of `a`, `a.b`, or `a.b.c`; none for any other shape.
fn path_last(e: &Expr) -> Option<TokSpan> {
    match e {
        Expr::Name(n) => Some(*n),

        Expr::Index {
            object,
            key: IndexKey::Field(f),
            optional: false,
            ..
        } => path_last(object).map(|_| *f),

        _ => None,
    }
}

/// Whether Luau text is one term that an operator beside it cannot
/// split: a name, a literal, a call, an index, or a bracketed group.
/// `1 + 2` is not, so `$add(1, 2) * 3` needs it in parentheses.
fn is_one_term(text: &str) -> bool {
    use alloy_syntax::lexer::TokKind;

    let Ok(lexed) = alloy_syntax::lexer::lex_luau(text) else {
        return false;
    };
    let mut depth = 0usize;

    for t in &lexed.toks {
        let word = t.text(text);

        match (t.kind, word) {
            (TokKind::LParen, _) | (TokKind::Symbol, "[" | "{") => depth += 1,

            (TokKind::RParen, _) | (TokKind::Symbol, "]" | "}") => {
                depth = depth.saturating_sub(1);
            }

            _ if depth > 0 => {}

            (TokKind::Ident, "and" | "or" | "not" | "if" | "function") => return false,

            (TokKind::Symbol, _) => return false,

            _ => {}
        }
    }

    true
}

pub(crate) fn is_simple_text(t: &str) -> bool {
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"' || c == '\'')
}

impl<'s> Desugar<'s> {
    /*
    A macro expands by substitution and a second compile. The body's tokens
    are joined onto one line with each parameter replaced by its argument's
    source text, then parsed and rendered like any Alloy, so intrinsics
    and sugar inside the body work. A body with statements in expression
    position wraps in a closure.
    */
    pub(crate) fn expand_macro(
        &mut self,
        m: &MacroRef,
        name: &str,
        args: &[Expr],
        span: TokSpan,
    ) -> String {
        let anchor = self.byte_start(span);

        self.macro_arity(m, name, args.len(), span);

        // The expansion is textual, so a body that calls its own macro
        // expands without end: `$fact(n - 1)` never reaches `n <= 1`.
        // A call inside an argument, `$max($max(a, b), c)`, nests too,
        // so a name check cannot tell the two apart; a depth can.
        if self.options.macro_depth >= 16 {
            self.diagnose(
                span,
                &format!("macro `{name}` expands itself; a macro cannot recurse"),
            );

            return if self.macro_stmt {
                String::new()
            } else {
                "nil".to_string()
            };
        }

        let arg_texts: Vec<String> = args
            .iter()
            .map(|a| self.text_of(a.span()).to_string())
            .collect();
        let mut subst: HashMap<&str, String> = HashMap::new();

        for (i, p) in m.params.iter().enumerate() {
            let text = arg_texts
                .get(i)
                .cloned()
                .or_else(|| m.defaults.get(i).cloned().flatten())
                .unwrap_or("nil".to_string());
            subst.insert(p.as_str(), text);
        }

        // A pattern parameter binds its names to reads of the argument.
        for (i, pattern) in m.patterns.iter().enumerate() {
            let text = arg_texts
                .get(i)
                .cloned()
                .or_else(|| m.defaults.get(i).cloned().flatten())
                .unwrap_or("nil".to_string());

            for (bound, access) in pattern {
                let read = match access.starts_with(['.', '[']) {
                    true => format!("({text}){access}"),

                    false => format!("{access}({text})"),
                };
                subst.insert(bound.as_str(), read);
            }
        }

        let rest: Vec<String> = arg_texts.iter().skip(m.params.len()).cloned().collect();

        // Hygiene: a local the body declares gets a name of its own per
        // expansion, so an argument that names the caller's `tmp` never
        // reads the body's `tmp`.
        self.macro_serial += 1;
        let serial = self.macro_serial;
        let mut renames: HashMap<String, String> = HashMap::new();

        for name in body_locals(&m.body) {
            if !m.params.contains(&name) && !name.starts_with("__") {
                renames
                    .entry(name.clone())
                    .or_insert_with(|| format!("{name}__m{serial}"));
            }
        }

        // In expression position the macro's value is its last
        // expression. A body that ends in `return <value>` gives that
        // value, and the `return` drops out; any other `return` shape
        // leaves the call with no value to stand for.
        let mut body = m.body.as_str();
        let mut tail = m.tail.clone();

        if tail.is_none() {
            let found = body_return(body);

            match self.macro_stmt {
                // In statement position the body's statements stay
                // statements, so a `return` that ends the body returns
                // from the function the call sits in. Luau takes
                // `return` only as the last statement of a block, and
                // the expansion writes the body where the call stands.
                true => {
                    if let Some(at) = found.last_return_at().filter(|_| self.macro_followed) {
                        self.diagnose(
                            span,
                            &format!(
                                "`{name}` returns from the function; nothing can follow it in the block"
                            ),
                        );
                        // The report is the whole answer. Keeping the
                        // `return` would give one block two of them,
                        // and the checker would say so again over an
                        // artifact no one wrote.
                        body = body[..at].trim_end();
                    }
                }

                false => match found {
                    BodyReturn::Tail(at, value) => {
                        body = body[..at].trim_end();
                        tail = Some(value);
                    }

                    BodyReturn::Other { .. } => {
                        self.diagnose(
                            span,
                            &format!(
                                "the macro `{name}` returns a statement; in an expression its body must end in a value"
                            ),
                        );

                        return "nil".to_string();
                    }

                    // A body of one expression the parser read as a
                    // statement gives that expression. Behind the
                    // `return` of the nested compile a `new` lowers to
                    // its one-call form; alone it lowers to statements,
                    // and statements cannot stand in an expression.
                    BodyReturn::None if !body.is_empty() => match body_value(body) {
                        Some(value) => {
                            body = "";
                            tail = Some(value);
                        }

                        None => {
                            self.diagnose(
                                span,
                                &format!(
                                    "the macro `{name}` expands to statements; in an expression its body must end in a value"
                                ),
                            );

                            return "nil".to_string();
                        }
                    },

                    BodyReturn::None => {}
                },
            }
        }

        // Substitute whole tokens in the one-line body, each behind the
        // gap the declaration wrote.
        let substitute = |text: &str| -> String {
            let mut out = String::new();
            let parts = body_parts(text);
            let mut open: Vec<&str> = Vec::new();

            for (i, &(gap, word)) in parts.iter().enumerate() {
                out.push_str(gap);
                let prev = i.checked_sub(1).map_or("", |j| parts[j].1);
                let next = parts.get(i + 1).map_or("", |p| p.1);

                // A name after `.` or `:` is a field or a method, and a
                // name before `=` in a table is a key. Neither is the
                // parameter or the local of that name.
                let field = matches!(prev, "." | ":")
                    || (open.last() == Some(&"{")
                        && matches!(prev, "{" | "," | ";")
                        && next == "=");

                match word {
                    "{" | "(" | "[" => open.push(word),

                    "}" | ")" | "]" => {
                        open.pop();
                    }

                    _ => {}
                }

                if field {
                    out.push_str(word);
                } else if word == "..." && m.variadic {
                    // Nothing behind the vararg: the comma in front of
                    // it has no argument to separate, and `f(a, )` is
                    // not Luau.
                    if rest.is_empty() {
                        while out.ends_with([' ', ',']) {
                            out.pop();
                        }

                        continue;
                    }

                    out.push_str(&rest.join(", "));
                } else if let Some(a) = subst.get(word)
                    && word.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    if is_simple_text(a) {
                        out.push_str(a);
                    } else {
                        out.push_str(&format!("({a})"));
                    }
                } else if let Some(r) = renames.get(word) {
                    out.push_str(r);
                } else {
                    out.push_str(word);
                }
            }

            out
        };

        let stmts = substitute(body);
        let tail = tail.as_deref().map(substitute);

        // A body that expands to nothing, called as a statement, writes
        // nothing: `nil` alone is no Luau statement, and the artifact
        // has to parse. A body whose trailing `return` came off above
        // lands here.
        if stmts.is_empty() && tail.is_none() && self.macro_stmt {
            return String::new();
        }

        let source = match (stmts.is_empty(), &tail) {
            (true, Some(t)) => t.clone(),

            (false, None) => stmts.clone(),

            (false, Some(t)) => format!("(function() {stmts} return {t} end)()"),

            (true, None) => "nil".to_string(),
        };

        let as_expr = stmts.is_empty() || tail.is_some();
        let nested_src = if as_expr {
            format!("return {source}")
        } else {
            source
        };

        // The nested compile sees this file's macros, one level down.
        self.options.macro_depth += 1;
        let out = self.compile_fragment(&nested_src, anchor, as_expr);
        self.options.macro_depth -= 1;

        out
    }

    /// The count a macro call has to give. A substitution has no call to
    /// check, so an argument too many is dropped and one too few becomes
    /// `nil`: the report has to come from here.
    fn macro_arity(&mut self, m: &MacroRef, name: &str, given: usize, span: TokSpan) -> bool {
        let most = m.params.len();
        // A default makes a parameter optional, so it is not required.
        let least = m.defaults.iter().filter(|d| d.is_none()).count();
        let open = m.variadic || least != most;

        if given >= least && (m.variadic || given <= most) {
            return true;
        }

        let wanted = if given < least { least } else { most };
        let message = format!(
            "the macro `{name}` takes {}{wanted} argument{}, {given} given",
            match (open, given < least) {
                (true, true) => "at least ",
                (true, false) => "at most ",
                (false, _) => "",
            },
            if wanted == 1 { "" } else { "s" }
        );
        self.diagnose(span, &message);

        false
    }

    /// Compiles a piece of Alloy on its own and splices the Luau in: the
    /// body of a macro with its arguments in place, or the match an
    /// intrinsic builds. The piece sees globals and what it names; it
    /// lands in the calling scope, where the names resolve.
    pub(crate) fn compile_fragment(
        &mut self,
        nested_src: &str,
        anchor: u32,
        as_expr: bool,
    ) -> String {
        let mut macros: Vec<MacroSource> = self
            .macros
            .iter()
            .map(|(name, r)| MacroSource {
                name: name.clone(),
                hidden: false,
                params: r.params.clone(),
                defaults: r.defaults.clone(),
                patterns: r.patterns.clone(),
                variadic: r.variadic,
                body: r.body.clone(),
                tail: r.tail.clone(),
            })
            .collect();

        // The fragment is the expansion, so a private macro of an
        // imported module is callable in it. A name this file already
        // binds keeps its own macro.
        for m in self.options.macros.iter().filter(|m| m.hidden) {
            if !macros.iter().any(|had| had.name == m.name) {
                macros.push(MacroSource {
                    hidden: false,
                    ..m.clone()
                });
            }
        }

        match crate::compile_with(
            nested_src,
            &EmitOptions {
                file_name: self.options.file_name.clone(),
                macros,
                // The enums of this file. The fragment is its own
                // compile, so a `match` in the body covers them only
                // when the index travels with it.
                macro_enums: self
                    .enum_decls
                    .iter()
                    .map(|(name, variants)| (name.clone(), variants.clone()))
                    .collect(),
                // The structs of this file travel as imported shapes:
                // a `new` in the body then lowers to the one-call form
                // and its fields check, as a `new` in this file does.
                import_struct_fields: self
                    .options
                    .import_struct_fields
                    .iter()
                    .cloned()
                    .chain(
                        self.struct_fields
                            .iter()
                            .map(|(n, f)| (n.clone(), f.clone())),
                    )
                    .collect(),
                // The outer file already binds every global it names;
                // the fragment lands inside it and needs no prologue.
                ..self.options.clone()
            },
        ) {
            Ok(out) => {
                if out.uses_std {
                    self.uses_std = true;
                }

                for d in out.diagnostics {
                    // One prefix says where the report comes from. A
                    // report from a deeper level carries it already.
                    let message = if d.message.starts_with("in macro expansion: ") {
                        d.message
                    } else {
                        format!("in macro expansion: {}", d.message)
                    };
                    self.diagnostics.push(Diagnostic {
                        start: anchor,
                        end: anchor,
                        message,
                    });
                }

                let text = out.ship.replace('\n', " ");
                let prefix = format!(
                    "local __alloy = require({}) ",
                    luau_string(&self.options.std_require)
                );
                let text = text
                    .strip_prefix(&prefix)
                    .unwrap_or(&text)
                    .trim()
                    .to_string();

                if !as_expr {
                    return text;
                }

                match text.strip_prefix("return ") {
                    Some(value) if is_one_term(value) => value.to_string(),

                    // The value lands between the operators around the
                    // call, so it keeps its own grouping.
                    Some(value) => format!("({value})"),

                    // Statements before the value, a hoisted temp: a
                    // closure keeps them in expression position.
                    None => format!("(function() {text} end)()"),
                }
            }

            Err(e) => {
                self.diagnostics.push(Diagnostic {
                    start: anchor,
                    end: anchor,
                    message: format!("macro expansion failed: {e}"),
                });

                "nil".to_string()
            }
        }
    }

    /// The intrinsics: a closed set, resolved by name.
    /// The report a dotted macro name earns, when the path reaches no
    /// macro. `None` for a bare name, which no namespace explains.
    fn macro_path_error(&self, name: &str) -> Option<String> {
        let (owner, member) = name.rsplit_once('.')?;

        Some(match self.is_namespace_path(owner) {
            true => format!("`{owner}` declares no macro `{member}`"),

            false => format!("`{owner}` is no namespace, so `${name}` names no macro"),
        })
    }

    pub(crate) fn intrinsic(&mut self, name: TokSpan, args: &[Expr], span: TokSpan) -> String {
        let n = self.text_of(name).to_string();
        let at = self.byte_start(span);
        let where_ = self.where_at(at);
        let rendered: Vec<String> = args.iter().map(|a| self.render_to_string(a)).collect();
        let sources: Vec<String> = args
            .iter()
            .map(|a| self.text_of(a.span()).to_string())
            .collect();

        match (n.as_str(), args.len()) {
            ("dbg", 1) => {
                let std = self.std();

                format!(
                    "{std}.dbg({}, {}, {})",
                    luau_string(&where_),
                    luau_string(&sources[0]),
                    rendered[0]
                )
            }

            ("todo", 0) => format!("error({})", luau_string(&format!("todo at {where_}"))),

            ("todo", 1) => format!(
                "error({} .. {})",
                luau_string(&format!("todo at {where_}: ")),
                rendered[0]
            ),

            ("unreachable", 0) => {
                format!(
                    "error({})",
                    luau_string(&format!("unreachable at {where_}"))
                )
            }

            ("assert", 1) => format!(
                "assert({}, {})",
                rendered[0],
                luau_string(&format!("assertion failed: {}", sources[0]))
            ),

            ("assert", 2) => format!("assert({}, {})", rendered[0], rendered[1]),

            ("assert_eq", 2) => {
                let std = self.std();

                format!(
                    "{std}.assert_eq({}, {}, {}, {}, {})",
                    luau_string(&where_),
                    luau_string(&sources[0]),
                    rendered[0],
                    luau_string(&sources[1]),
                    rendered[1]
                )
            }

            // `$nameof(a.b.c)` is `"c"`. A call, a literal, an operator,
            // or a bracket index has no name to take, so it reports and
            // writes `nil`, which still parses.
            ("nameof", 1) => match path_last(&args[0]) {
                Some(last) => luau_string(self.text_of(last)),

                None => {
                    let text = sources[0].trim();
                    self.diagnose(
                        args[0].span(),
                        &format!("`$nameof` takes a name or a dotted path; `{text}` is not one"),
                    );

                    "nil".to_string()
                }
            },

            ("stringify", 1) => luau_string(&sources[0]),

            // `$expect(value)` is the runner's expectation object, so
            // every matcher is a method call on it. The object exists
            // under the runner alone: outside a `@test` nothing builds
            // one, and a project with no runner has none to build.
            ("expect", 1) => {
                if !crate::testbuild::encloses_test(self.src, at as usize) {
                    self.diagnose(
                        span,
                        "`$expect` needs `@test` on the function or the namespace around it",
                    );
                } else if !self.options.test_runner {
                    self.diagnose(
                        span,
                        "`$expect` needs a test runner; the project sets `[test] lest = false`",
                    );
                }

                let std = self.std();

                format!("{std}.expect({})", rendered[0])
            }

            ("bnot", 1) => format!("bit32.bnot({})", rendered[0]),

            // `$matches(value, Pattern)`: the match with one arm, as a
            // boolean. The pattern is the second argument's text, read
            // by the match parser.
            ("matches", 2) => {
                let nested = format!(
                    "return match {} with case {} then true default false end",
                    sources[0], sources[1]
                );

                self.compile_fragment(&nested, at, true)
            }

            // `$set[a, b]` or `$set(a, b)`: a Set of the values.
            ("set", _) => {
                let std = self.std();
                let items: Vec<String> = match args {
                    [Expr::Array { items, .. }] => {
                        items.iter().map(|e| self.render_to_string(e)).collect()
                    }

                    _ => rendered,
                };

                let text = format!("{std}.Set.from({{ {} }})", items.join(", "));

                // An empty set has no item to name its type; the binding or
                // the field it lands in names it.
                match items.is_empty() {
                    true => self.any_cast(&text),

                    false => text,
                }
            }

            // `$map[[k, v], ...]` or `$map([k, v], ...)`: a HashMap of
            // the pairs.
            ("map", _) => {
                let std = self.std();
                // `$map[...]` holds the pairs in one bracket list;
                // `$map(...)` passes each pair as an argument, so a lone
                // `[key, value]` there is one pair.
                let head = self.text_of(span);
                let bracket = head
                    .find(['[', '('])
                    .is_some_and(|i| head.as_bytes()[i] == b'[');
                let pairs: &[Expr] = match args {
                    [
                        Expr::Array {
                            items, span: list, ..
                        },
                    ] if bracket => {
                        if items.iter().all(|e| matches!(e, Expr::Array { .. })) {
                            items
                        } else {
                            // `$map["k", v]`: a flat list names one key
                            // and one value where a pair belongs.
                            self.diagnose(
                                *list,
                                "`$map` takes pairs: `$map[[key, value], [key, value]]`",
                            );

                            &[]
                        }
                    }

                    _ => args,
                };
                let mut fields = Vec::new();
                // A table literal with string keys reads as a record, so
                // the checker learns `K` and `V` from a cast: the types
                // of the first pair, a literal's own or `typeof` of the
                // expression.
                let mut shape = None;

                for pair in pairs {
                    match pair {
                        Expr::Array { items, .. } if items.len() == 2 => {
                            let k = self.render_to_string(&items[0]);
                            let v = self.render_to_string(&items[1]);

                            if shape.is_none() {
                                let kt = literal_type(&items[0])
                                    .unwrap_or_else(|| format!("typeof({k})"));
                                let vt = literal_type(&items[1])
                                    .unwrap_or_else(|| format!("typeof({v})"));
                                shape = Some(format!("{{ [{kt}]: {vt} }}"));
                            }

                            fields.push(format!("[{k}] = {v}"));
                        }

                        other => {
                            self.diagnose(
                                other.span(),
                                "`$map` takes pairs: `$map[[key, value], [key, value]]`",
                            );
                        }
                    }
                }

                match shape {
                    Some(shape) => {
                        format!("{std}.HashMap.from({{ {} }} :: {shape})", fields.join(", "))
                    }

                    None => self.any_cast(&format!("{std}.HashMap.from({{}})")),
                }
            }

            _ => {
                let message = self
                    .macro_path_error(&n)
                    .or_else(|| self.ns_member_hint(&self.macros, &n, "a macro", '$'))
                    .unwrap_or_else(|| {
                        format!(
                            "unknown macro or intrinsic `${n}` with {} arguments",
                            args.len()
                        )
                    });
                self.diagnose(span, &message);

                self.text_of(span).to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// `$map` takes pairs. A flat list reads as one pair and built a
    /// map of one entry in silence.
    #[test]
    fn a_flat_map_literal_is_an_error() {
        let got = messages("local m = $map[\"sword\", 10]\nprint(m)\n");
        assert_eq!(
            got,
            vec!["`$map` takes pairs: `$map[[key, value], [key, value]]`"]
        );
        assert!(messages("local m = $map[[\"sword\", 10]]\nprint(m)\n").is_empty());
        assert!(messages("local m = $map[]\nprint(m)\n").is_empty());
    }

    /// `$expect(v)` builds the runner's expectation object, so it
    /// needs `@test` above it and a runner in the project. A helper a
    /// test calls carries no `@test`, so it reports: nothing proves
    /// only a test reaches it.
    #[test]
    fn expect_needs_a_test_and_a_runner() {
        let test = "@test\nfunction case()\n    $expect(1):toBe(1)\nend\n";
        assert!(messages(test).is_empty(), "{:?}", messages(test));
        assert!(
            crate::compile(test)
                .unwrap()
                .check
                .contains("__alloy.expect(1):toBe(1)"),
            "{}",
            crate::compile(test).unwrap().check
        );

        let group = "@test namespace Suite as\n    public function case()\n        $expect(1):toBe(1)\n    end\n\n    private function helper()\n        $expect(2):toBe(2)\n    end\nend\n";
        assert!(messages(group).is_empty(), "{:?}", messages(group));

        let helper = "function helper()\n    $expect(1):toBe(1)\nend\n";
        assert_eq!(
            messages(helper),
            vec!["`$expect` needs `@test` on the function or the namespace around it"]
        );

        let options = crate::EmitOptions {
            test_runner: false,
            ..crate::EmitOptions::default()
        };
        let out = crate::compile_with(test, &options).unwrap();
        assert_eq!(
            out.diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>(),
            vec!["`$expect` needs a test runner; the project sets `[test] lest = false`"]
        );
    }

    /// `$M.twice(2)` reads the macro of a namespace. A path that
    /// reaches none names the path, not the intrinsic list.
    #[test]
    fn a_macro_path_reads_the_namespace() {
        let src = "namespace M as\n    macro twice(x)\n        x * 2\n    end\nend\n\nprint($M.twice(2))\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));

        let gone = src.replace("$M.twice(2)", "$M.nope(2)");
        assert_eq!(messages(&gone), vec!["`M` declares no macro `nope`"]);

        let head = "print($Nope.thing(1))\n";
        assert_eq!(
            messages(head),
            vec!["`Nope` is no namespace, so `$Nope.thing` names no macro"]
        );
    }

    /// A macro body compiles as a fragment of its own, so it needs the
    /// enums of the file it expands in. Without them a `match` over one
    /// covered nothing and reported.
    #[test]
    fn a_match_in_a_macro_body_covers_the_enum_of_the_file() {
        let src = "enum Choice as\n    Yes\n    No\nend\n\nmacro describe(c)\n    local r = match c with\n        case Choice.Yes then \"yes\"\n        case Choice.No then \"no\"\n    end\n    r\nend\n\nlocal function show(x: Choice): string\n    return $describe(x)\nend\n\nprint(show(Choice.Yes))\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));

        // An arm that leaves a variant out still reports.
        let one = src.replace("        case Choice.No then \"no\"\n", "");
        assert_eq!(
            messages(&one),
            vec![
                "in macro expansion: this match is not exhaustive: `Choice` has no arm for `No`; add it or a `default` arm".to_string()
            ]
        );
    }

    /// A body of one `new Pt { ... }` is a statement to the parser. In
    /// expression position the expansion wrote the statements a `new`
    /// alone gets, `local p = local _n1 = ...`, and Luau refused it.
    #[test]
    fn a_struct_literal_macro_is_a_value_in_expression_position() {
        let decl = "macro origin() new Pt { x = 0, y = 0 } end\n\n";
        let calls = [
            "local p = $origin()\nprint(p)\n",
            "print($origin())\n",
            "local s = match 1 with\n    case 0 then tostring($origin())\n    default \"n\"\nend\nprint(s)\n",
        ];

        for call in calls {
            let out = crate::compile(&format!("{decl}{call}")).unwrap();

            assert!(out.diagnostics.is_empty(), "{call}: {:?}", out.diagnostics);
            assert!(
                out.ship.contains("__alloy.construct(Pt, { x = 0, y = 0 })"),
                "{call}: {}",
                out.ship
            );
            assert!(!out.ship.contains("local _n1"), "{call}: {}", out.ship);
        }

        // Alone, the call is a statement and the body stays one.
        let out = crate::compile(&format!("{decl}$origin()\n")).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("local _n1 = "), "{}", out.ship);

        // A body of two statements has no value, and the call says so
        // instead of shipping statements Luau cannot read.
        assert_eq!(
            messages(
                "macro two(x)\n    print(x)\n    print(x)\nend\n\nlocal t = $two(1)\nprint(t)\n"
            ),
            vec![
                "the macro `two` expands to statements; in an expression its body must end in a value"
            ]
        );

        // A struct this file declares reaches the body with its fields.
        assert_eq!(
            messages(
                "struct Own as\n    x: number\nend\n\nmacro bad() new Own { z = 1 } end\n\nlocal o = $bad()\nprint(o)\n"
            ),
            vec![
                "in macro expansion: `new Own { ... }` leaves `x` unset; a field without a default needs a value",
                "in macro expansion: `Own` has no field `z`; its fields are `x`",
            ]
        );
    }

    /// The expansion is textual, so a macro that calls itself expands
    /// until the parser gives up on the nesting. The report was one
    /// `in macro expansion:` per level, about 58 of them, in front of
    /// the parser's own words.
    #[test]
    fn a_macro_that_expands_itself_reports_once() {
        let src = "macro fact(n) n <= 1 ? 1 : n * $fact(n - 1) end\n\nprint($fact(5))\n";

        assert_eq!(
            messages(src),
            vec!["in macro expansion: macro `fact` expands itself; a macro cannot recurse"]
        );

        // Two macros that call each other are one loop.
        let src = "macro ping(n) $pong(n) end\nmacro pong(n) $ping(n) end\n\nprint($ping(1))\n";

        assert_eq!(
            messages(src),
            vec!["in macro expansion: macro `ping` expands itself; a macro cannot recurse"]
        );

        // A call inside an argument nests without a loop.
        let src =
            "macro twice(n) n * 2\nend\nmacro four(n) $twice($twice(n)) end\n\nprint($four(1))\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("(((1 * 2)) * 2)"), "{}", out.ship);
    }

    /// A macro body travels as one line. The join used to put a space
    /// between every pair of tokens, so the expansion wrote
    /// `Choice . Yes`, `s : upper ( )` and `t [ 1 ]`. The gap the
    /// declaration wrote now decides the space.
    #[test]
    fn a_macro_body_keeps_the_spacing_the_source_wrote() {
        let decl = "enum Choice as\n    Yes\n    No\nend\n\nmacro pick(c)\n    c == Choice.Yes\nend\n\nmacro shout(s)\n    s:upper()\nend\n\nmacro first(t)\n    t[1]\nend\n\n";
        let src = format!(
            "{decl}local c: Choice = Choice.Yes\nprint($pick(c))\nprint($shout(\"hi\"))\nprint($first({{ 1, 2 }}))\n"
        );
        let out = crate::compile(&src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("c == Choice.Yes"), "{}", out.ship);
        assert!(out.ship.contains("(\"hi\"):upper()"), "{}", out.ship);
        assert!(out.ship.contains("({ 1, 2 })[1]"), "{}", out.ship);
        assert!(!out.ship.contains(" . "), "{}", out.ship);

        // A parameter the source writes tight still substitutes, since
        // the split is the lexer's, not the space's.
        let src = "macro call(f, x)\n    f(x)\nend\n\nprint($call(tostring, 1))\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("tostring(1)"), "{}", out.ship);

        // A name inside a string is no token, so it never substitutes.
        let src = "macro say(x)\n    print(\"x here\", x)\nend\n\n$say(1)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("print(\"x here\", 1)"), "{}", out.ship);
    }

    /// A macro substitutes; there is no call for the checker to count.
    /// An argument too many was dropped and one too few became `nil`,
    /// both without a word.
    #[test]
    fn a_macro_call_counts_its_arguments() {
        let decl = "macro clamp01(x)\n    math.clamp(x, 0, 1)\nend\n\n";
        assert_eq!(
            messages(&format!("{decl}local c = $clamp01(5, 6)\nprint(c)\n")),
            vec!["the macro `clamp01` takes 1 argument, 2 given"]
        );
        assert_eq!(
            messages(&format!("{decl}local c = $clamp01()\nprint(c)\n")),
            vec!["the macro `clamp01` takes 1 argument, 0 given"]
        );
        assert!(messages(&format!("{decl}local c = $clamp01(5)\nprint(c)\n")).is_empty());

        // A variadic macro takes the named parameters and any number
        // after them.
        let variadic = "macro log(tag, ...)\n    print(tag, ...)\nend\n\n";
        assert_eq!(
            messages(&format!("{variadic}$log()\n")),
            vec!["the macro `log` takes at least 1 argument, 0 given"]
        );
        assert!(messages(&format!("{variadic}$log(\"a\", 1, 2)\n")).is_empty());

        // A vararg with nothing behind it left `print(tag, )`.
        assert!(messages(&format!("{variadic}$log(\"a\")\n")).is_empty());

        // A default makes the parameter optional, and the default's
        // own text stands in for the argument it replaces.
        let optional = "macro retry_count(n = 3)\n    n\nend\n\n";
        assert!(messages(&format!("{optional}local t = $retry_count()\nprint(t)\n")).is_empty());
        assert!(messages(&format!("{optional}local t = $retry_count(5)\nprint(t)\n")).is_empty());
        assert_eq!(
            messages(&format!(
                "{optional}local t = $retry_count(5, 6)\nprint(t)\n"
            )),
            vec!["the macro `retry_count` takes at most 1 argument, 2 given"]
        );
        assert!(
            crate::compile(&format!("{optional}local t = $retry_count()\nprint(t)\n"))
                .unwrap()
                .ship
                .contains("local t = 3"),
            "the default did not stand in"
        );

        let none = "macro tick()\n    print(1)\nend\n\n";
        assert_eq!(
            messages(&format!("{none}$tick(1)\n")),
            vec!["the macro `tick` takes 0 arguments, 1 given"]
        );
    }

    /*
    A macro body that ends in `return <value>`. In expression position
    the expansion spliced the statement in whole, so `local n = $sum(1)`
    emitted `local n = return 1 + 2`, which is no Luau.

    The value of such a body is the expression the `return` carries, and
    the word drops out. A body with any other `return` shape has no
    value for an expression, and the call reports.
    */
    /// A body that is one if-expression is the value; a body that is
    /// an if-statement stays a statement.
    #[test]
    fn a_macro_body_that_is_an_if_expression_is_its_value() {
        let src = "macro pick(c, a, b)\n    if c then a else b\nend\n\nmacro either(c)\n    if c then print(\"a\") else print(\"b\") end\nend\n\nlocal p = $pick(true, 1, 2)\n$either(false)\nprint(p)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("local p = (if true then 1 else 2)"),
            "{}",
            out.ship
        );
        assert!(
            out.ship
                .contains("if false then print(\"a\") else print(\"b\") end"),
            "{}",
            out.ship
        );
    }

    /// The value lands between the operators around the call, so it
    /// keeps its own grouping: `10 / $sq(2)` is 2.5, not 10.
    #[test]
    fn a_macro_value_keeps_its_grouping() {
        let src = "macro sq(x) (x) * (x) end\nmacro add(a, b) a + b end\nmacro first(t) t[1] end\nlocal t = { 4 }\nprint(10 / $sq(2), $add(1, 2) * 3, -$add(1, 2), $first(t))\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("print(10 / ((2) * (2)), (1 + 2) * 3, -(1 + 2), t[1])"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_macro_body_that_returns_a_value_gives_it_to_an_expression() {
        let decl = "macro sum(a, b = 2)\n    return a + b\nend\n\n";
        let ship = |src: &str| -> String { crate::compile(src).unwrap().ship };

        // Both arities: the default stands in for the argument it
        // replaces, and the `return` is gone.
        let out = ship(&format!(
            "{decl}local n = $sum(1)\nlocal m = $sum(1, 5)\nprint(n, m)\n"
        ));

        assert!(out.contains("local n = (1 + 2)"), "{out}");
        assert!(out.contains("local m = (1 + 5)"), "{out}");
        assert!(!out.contains("= return"), "{out}");

        // A statement-position call keeps the `return`, so the body
        // returns from the function around the call.
        let out = ship(&format!(
            "{decl}local function one(): number\n    $sum(1)\nend\nprint(one())\n"
        ));

        assert!(out.contains("return 1 + 2"), "{out}");

        // An early `return`, and a `return` with no value: neither has
        // a value for an expression.
        let early = "macro pick(c)\n    if c then return 1 end\n    return 2\nend\n\n";
        assert_eq!(
            messages(&format!("{early}local p = $pick(true)\nprint(p)\n")),
            vec![
                "the macro `pick` returns a statement; in an expression its body must end in a value"
            ]
        );

        let bare = "macro stop()\n    return\nend\n\n";
        assert_eq!(
            messages(&format!("{bare}local s = $stop()\nprint(s)\n")),
            vec![
                "the macro `stop` returns a statement; in an expression its body must end in a value"
            ]
        );

        // The same bodies stand as statements.
        assert!(messages(&format!("{early}$pick(true)\n")).is_empty());
        assert!(messages(&format!("{bare}$stop()\n")).is_empty());

        // A body with statements in front of the value still wraps in a
        // closure, and the value is the `return`'s expression.
        let mixed = "macro noisy(x)\n    print(x)\n    return x + 1\nend\n\n";
        let out = ship(&format!("{mixed}local v = $noisy(2)\nprint(v)\n"));

        assert!(out.contains("return 2 + 1"), "{out}");
        assert!(out.contains("print(2)"), "{out}");
    }

    /*
    A macro body that ends in `return`, called as a statement with more
    of the block behind it. The expansion wrote the `return` where the
    call stood, so one block held two `return` statements and Luau
    rejected the output.

    Such a body returns from the function the call sits in, so the call
    has to be the last statement of its block.
    */
    #[test]
    fn a_macro_that_returns_reports_when_a_statement_follows_it() {
        let decl = "macro give_up()\n    return 0\nend\n\n";
        let call = |after: &str| {
            format!(
                "{decl}local function work(): number\n    $give_up()\n{after}end\nprint(work())\n"
            )
        };

        assert_eq!(
            messages(&call("    return 1\n")),
            vec!["`give_up` returns from the function; nothing can follow it in the block"]
        );

        // Last in its block: the body's `return` is the function's.
        let out = crate::compile(&call("")).unwrap().ship;

        assert!(out.contains("return 0"), "{out}");
        assert!(messages(&call("")).is_empty());

        // A body whose last statement is a bare `return` reports too.
        let bare = "macro stop()\n    return\nend\n\n";

        assert_eq!(
            messages(&format!("{bare}$stop()\nprint(1)\n")),
            vec!["`stop` returns from the function; nothing can follow it in the block"]
        );

        // A body with no `return` at its end takes any block position.
        let quiet = "macro note()\n    print(1)\nend\n\n";

        assert!(messages(&format!("{quiet}$note()\nprint(2)\n")).is_empty());
    }

    /*
    The report is the whole answer: the artifact still has to parse, so
    the checker adds nothing over an emit no one wrote.

    The expansion kept the `return`, so the block held two of them and
    `alloy flux` printed two raw checker syntax errors behind the one
    report. The `return` now comes off, and the rest of the body stays.
    */
    /// `$nameof` takes a name or a dotted path and writes the last name.
    /// It took any expression and wrote its text, so `$nameof(f())` was
    /// `"f()"`. Now it reports and writes `nil`.
    #[test]
    fn nameof_takes_a_name_or_a_dotted_path() {
        let src = "local a = { b = { c = 1 } }\nlocal M = { Ns = { T = 1 } }\nprint($nameof(a), $nameof(a.b.c), $nameof(M.Ns.T))\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("print(\"a\", \"c\", \"T\")"),
            "{}",
            out.check
        );

        let src = "local function f() return 1 end\nlocal t = { 1 }\nprint($nameof(f()), $nameof(\"literal\"), $nameof(1), $nameof(t[1]), $nameof(1 + 2))\n";
        let out = crate::compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            messages,
            vec![
                "`$nameof` takes a name or a dotted path; `f()` is not one",
                "`$nameof` takes a name or a dotted path; `\"literal\"` is not one",
                "`$nameof` takes a name or a dotted path; `1` is not one",
                "`$nameof` takes a name or a dotted path; `t[1]` is not one",
                "`$nameof` takes a name or a dotted path; `1 + 2` is not one",
            ]
        );
        assert!(
            out.check.contains("print(nil, nil, nil, nil, nil)"),
            "{}",
            out.check
        );
        // The report sits on the argument, not on the whole call, and
        // reads as a MacroError.
        let at = (src.find("$nameof(f())").unwrap() + "$nameof(".len()) as u32;
        assert_eq!(out.diagnostics[0].start, at);
        assert_eq!(crate::docs::kind_for(messages[0]), "MacroError");
    }

    #[test]
    fn a_reported_macro_return_leaves_the_artifact_parsing() {
        let decl = "macro give_up()\n    print(\"bye\")\n    return 0\nend\n\n";
        let src = format!(
            "{decl}local function work(): number\n    $give_up()\n    return 1\nend\n\nprint(work())\n"
        );
        let out = crate::compile(&src).unwrap();

        assert_eq!(
            out.diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<String>>(),
            vec!["`give_up` returns from the function; nothing can follow it in the block"]
        );
        // One `return` in the block, and the body's other statement.
        assert!(out.check.contains("print(\"bye\")"), "{}", out.check);
        assert_eq!(out.check.matches("return").count(), 1, "{}", out.check);

        // A body that is the `return` alone writes nothing: `nil` is no
        // Luau statement.
        let bare = "macro stop()\n    return\nend\n\n";
        let out = crate::compile(&format!("{bare}$stop()\nprint(1)\n")).unwrap();

        assert!(!out.check.contains("nil"), "{}", out.check);
        assert!(!out.check.contains("return"), "{}", out.check);
    }
}
