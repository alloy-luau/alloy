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
    pub(crate) variadic: bool,
    /// The statements, tokens joined by spaces.
    pub(crate) body: String,
    /// The trailing expression, tokens joined by spaces.
    pub(crate) tail: Option<String>,
}

/// Text that reads the same with or without parentheses around it.
/// The names a macro body declares: after `local`, the variables of a
/// `for`, and the parameters of a `function` inside it. The body is one
/// line of tokens joined by spaces.
pub(crate) fn body_locals(body: &str) -> Vec<String> {
    let words: Vec<&str> = body.split(' ').collect();
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

        // Substitute whole words in the one-line body.
        let substitute = |text: &str| -> String {
            let mut out = String::new();

            for word in text.split(' ') {
                if !out.is_empty() {
                    out.push(' ');
                }

                if word == "..." && m.variadic {
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

        let stmts = substitute(&m.body);
        let tail = m.tail.as_ref().map(|t| substitute(t));

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
        self.compile_fragment(&nested_src, anchor, as_expr)
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

        if self.options.macros.iter().filter(|m| !m.hidden).count() > 16 {
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: anchor,
                message: "macro expansion nests too deeply".to_string(),
            });

            return "nil".to_string();
        }

        match crate::compile_with(
            nested_src,
            &EmitOptions {
                file_name: self.options.file_name.clone(),
                macros,
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
                    self.diagnostics.push(Diagnostic {
                        start: anchor,
                        end: anchor,
                        message: format!("in macro expansion: {}", d.message),
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
                    Some(value) => value.to_string(),

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

            ("nameof", 1) => {
                let last = sources[0].rsplit(['.', ':']).next().unwrap_or(&sources[0]);

                luau_string(last.trim())
            }

            ("stringify", 1) => luau_string(&sources[0]),

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

                format!("{std}.Set.from({{ {} }})", items.join(", "))
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

                    None => format!("{std}.HashMap.from({{}})"),
                }
            }

            _ => {
                self.diagnose(
                    span,
                    &format!(
                        "unknown macro or intrinsic `${n}` with {} arguments",
                        args.len()
                    ),
                );

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
}
