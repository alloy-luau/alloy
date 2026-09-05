//! Flux: the complexity and perf lints. A function with too many
//! parameters, lines, or branches; a nest of `if`s that is one `if`;
//! work inside a loop that belongs above it. The limits come from
//! `[flux]`; the names and levels sit in `lint::LINTS`.

use crate::flux_scan::{IfParts, Scan};
use crate::lint::{Lint, Thresholds};

/// Runs the complexity and perf lints on one file.
pub(crate) fn run(s: &Scan, limits: &Thresholds) -> Vec<Lint> {
    let mut out = Vec::new();
    s.too_many_arguments(&mut out, limits.too_many_arguments);
    s.too_many_lines(&mut out, limits.too_many_lines);
    s.deep_nesting(&mut out, limits.max_nesting);
    s.cognitive_complexity(&mut out, limits.cognitive_complexity);
    s.collapsible_if(&mut out);
    s.collapsible_else_if(&mut out);
    s.concat_in_loop(&mut out);
    s.service_in_loop(&mut out);
    s.table_insert_position(&mut out);
    out
}

impl<'s> Scan<'s> {
    /// The `function` tokens that open a body: the token and its `end`.
    fn functions(&self) -> Vec<(usize, usize)> {
        (0..self.toks.len())
            .filter(|&i| self.at(i, "function") && !matches!(self.prev(i), "." | ":"))
            .filter_map(|i| self.st.ends[i].map(|e| (i, e)))
            .collect()
    }

    /// The name of a function for a message: its path, or `function`.
    fn function_name(&self, i: usize) -> String {
        let mut j = i + 1;

        while self.is_name(j) || self.at(j, ".") || self.at(j, ":") {
            j += 1;
        }

        if j == i + 1 {
            "this function".to_string()
        } else {
            format!("`{}`", self.slice(i + 1, j))
        }
    }

    /// A function with more parameters than the limit.
    fn too_many_arguments(&self, out: &mut Vec<Lint>, limit: usize) {
        for (i, _) in self.functions() {
            let mut open = i + 1;

            while self.is_name(open) || self.at(open, ".") || self.at(open, ":") {
                open += 1;
            }

            if !self.at(open, "(") {
                continue;
            }

            let Some(close) = self.matching(open) else {
                continue;
            };
            let mut count = 0;
            let mut depth = 0i32;
            let mut at_start = true;

            for j in open + 1..close {
                let text = self.t(j);

                if at_start && text != "self" {
                    count += 1;
                }

                at_start = false;

                if matches!(text, "(" | "[" | "{" | "<") {
                    depth += 1;
                } else if matches!(text, ")" | "]" | "}" | ">") {
                    depth -= 1;
                } else if text == "," && depth == 0 {
                    at_start = true;
                }
            }

            if count > limit {
                self.lint(
                    out,
                    "too_many_arguments",
                    i,
                    close,
                    format!(
                        "{} takes {count} parameters, past the limit of {limit}; group them in a struct or split the function",
                        self.function_name(i)
                    ),
                    None,
                );
            }
        }
    }

    /// A function body longer than the limit.
    fn too_many_lines(&self, out: &mut Vec<Lint>, limit: usize) {
        for (i, e) in self.functions() {
            let lines = self.line_of(e).saturating_sub(self.line_of(i) + 1);

            if lines > limit {
                self.lint(
                    out,
                    "too_many_lines",
                    i,
                    i,
                    format!(
                        "{} runs {lines} lines, past the limit of {limit}; name its parts and call them",
                        self.function_name(i)
                    ),
                    None,
                );
            }
        }
    }

    /// A block nested deeper than the limit. Only the block at the
    /// limit fires, not each one inside it.
    fn deep_nesting(&self, out: &mut Vec<Lint>, limit: usize) {
        let nest = self.nesting();

        for (i, e) in self.st.ends.iter().enumerate() {
            if e.is_none()
                || !matches!(
                    self.t(i),
                    "if" | "for" | "while" | "repeat" | "do" | "match"
                )
                || matches!(self.prev(i), "." | ":")
                || (self.at(i, "if") && !self.is_statement_if(i))
            {
                continue;
            }

            if nest[i] + 1 == limit + 1 {
                let word = self.t(i);
                self.lint(
                    out,
                    "deep_nesting",
                    i,
                    i,
                    format!(
                        "this `{word}` sits {} blocks deep, past the limit of {limit}; return early or move it into a function",
                        nest[i] + 1
                    ),
                    None,
                );
            }
        }
    }

    /// The cognitive complexity score of each function.
    fn cognitive_complexity(&self, out: &mut Vec<Lint>, limit: usize) {
        let nest = self.nesting();

        for (i, e) in self.functions() {
            let base = nest[i] + 1;
            let mut score = 0usize;

            for (j, &at) in nest.iter().enumerate().take(e).skip(i + 1) {
                let text = self.t(j);

                if matches!(self.prev(j), "." | ":") {
                    continue;
                }

                let depth = at.saturating_sub(base);

                score += match text {
                    "if" if self.is_statement_if(j) => 1 + depth,
                    "if" => 1,
                    "for" | "while" | "repeat" | "match" => 1 + depth,
                    "elseif" | "else" | "and" | "or" | "case" => 1,
                    "?" => 1,
                    _ => 0,
                };
            }

            if score > limit {
                self.lint(
                    out,
                    "cognitive_complexity",
                    i,
                    i,
                    format!(
                        "{} scores {score} for branches and nesting, past the limit of {limit}; split it where the deepest branches begin",
                        self.function_name(i)
                    ),
                    None,
                );
            }
        }
    }

    /// A condition, in parentheses when `or` or `?` at depth zero would
    /// bind wrong beside `and`.
    fn guarded(&self, a: usize, b: usize) -> String {
        let text = self.slice(a, b).trim().to_string();
        let mut depth = 0i32;

        for j in a..b {
            let t = self.t(j);

            if matches!(t, "(" | "[" | "{") || t.ends_with('(') || t.ends_with('[') {
                depth += 1;
            } else if matches!(t, ")" | "]" | "}") {
                depth -= 1;
            } else if depth == 0 && matches!(t, "or" | "?") {
                return format!("({text})");
            }
        }

        text
    }

    /// `if a then if b then ... end end` is `if a and b then ... end`.
    fn collapsible_if(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let Some(IfParts {
                then,
                elseifs,
                else_at: None,
                end,
            }) = self.if_parts(i)
            else {
                continue;
            };

            if !elseifs.is_empty() || !self.at(then + 1, "if") {
                continue;
            }

            let j = then + 1;
            let Some(IfParts {
                then: inner_then,
                elseifs: inner_elseifs,
                else_at: None,
                end: inner_end,
            }) = self.if_parts(j)
            else {
                continue;
            };

            if !inner_elseifs.is_empty()
                || inner_end + 1 != end
                || self.comment_between(then, j)
                || self.comment_between(inner_end, end)
            {
                continue;
            }

            let a = self.guarded(i + 1, then);
            let b = self.guarded(j + 1, inner_then);
            let body = &self.src[self.end(inner_then) as usize..self.start(inner_end) as usize];
            self.lint(
                out,
                "collapsible_if",
                i,
                end,
                format!("an `if` whose only statement is an `if` is one: `if {a} and {b} then`"),
                Some(format!("if {a} and {b} then{body}end")),
            );
        }
    }

    /// `else if ... end end` is `elseif ... end`.
    fn collapsible_else_if(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let Some(IfParts {
                else_at: Some(else_at),
                end,
                ..
            }) = self.if_parts(i)
            else {
                continue;
            };

            if !self.at(else_at + 1, "if") {
                continue;
            }

            let j = else_at + 1;
            let Some(IfParts { end: inner_end, .. }) = self.if_parts(j) else {
                continue;
            };

            if inner_end + 1 != end
                || self.comment_between(else_at, j)
                || self.comment_between(inner_end, end)
            {
                continue;
            }

            let rest = &self.src[self.end(j) as usize..self.end(inner_end) as usize];
            self.lint(
                out,
                "collapsible_else_if",
                else_at,
                end,
                "an `else` whose only statement is an `if` is an `elseif`".to_string(),
                Some(format!("elseif{rest}")),
            );
        }
    }

    /// `s = s .. x` inside a loop.
    fn concat_in_loop(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "=") || i == 0 {
                continue;
            }

            let c = self.expr_start_before(i);

            if !self.statement_start(c) || self.path_end(c) != Some(i) {
                continue;
            }

            let Some(q) = self.same_path(c, i, i + 1) else {
                continue;
            };

            if !self.at(q, "..") || !self.in_loop(i) {
                continue;
            }

            let path = self.slice(c, i);
            self.lint(
                out,
                "concat_in_loop",
                c,
                q,
                format!(
                    "`{path} = {path} .. ...` copies the whole string each time round the loop; push the pieces into a table and `table.concat` once"
                ),
                None,
            );
        }
    }

    /// `game:GetService(...)` inside a loop.
    fn service_in_loop(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if self.at(i, "game")
                && self.at(i + 1, ":")
                && self.at(i + 2, "GetService")
                && self.at(i + 3, "(")
                && self.in_loop(i)
            {
                let name = self.string_content(i + 4).unwrap_or("...");
                self.lint(
                    out,
                    "service_in_loop",
                    i,
                    i + 2,
                    format!("`game:GetService(\"{name}\")` runs every time round the loop; bind the service once above it"),
                    None,
                );
            }
        }
    }

    /// `table.insert(t, #t + 1, v)` is `table.insert(t, v)`.
    fn table_insert_position(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.at(i, "table")
                && self.at(i + 1, ".")
                && self.at(i + 2, "insert")
                && self.at(i + 3, "("))
            {
                continue;
            }

            let Some(close) = self.matching(i + 3) else {
                continue;
            };
            let Some(p) = self.path_end(i + 4) else {
                continue;
            };

            if !(self.at(p, ",") && self.at(p + 1, "#")) {
                continue;
            }

            let Some(q) = self.same_path(i + 4, p, p + 2) else {
                continue;
            };

            if !(self.at(q, "+") && self.at(q + 1, "1") && self.at(q + 2, ",")) {
                continue;
            }

            let table = self.slice(i + 4, p);
            let value = self.slice(q + 3, close).trim();
            self.lint(
                out,
                "table_insert_position",
                i,
                close,
                format!("`table.insert({table}, #{table} + 1, v)` is `table.insert({table}, v)`"),
                Some(format!("table.insert({table}, {value})")),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::{Thresholds, apply_fixes};

    fn lints(src: &str) -> Vec<crate::Lint> {
        crate::compile(src).unwrap().lints
    }

    fn fixed(src: &str) -> String {
        apply_fixes(src, &lints(src)).0
    }

    /// The lints at their default level: the pedantic ones stay out.
    fn names(src: &str) -> Vec<&'static str> {
        let config = crate::config::LintConfig::default();

        lints(src)
            .iter()
            .map(|l| l.name)
            .filter(|n| crate::lint::level_of(&config, n) != crate::lint::Level::Allow)
            .collect()
    }

    fn names_with(src: &str, thresholds: Thresholds) -> Vec<&'static str> {
        let options = crate::EmitOptions {
            thresholds,
            ..Default::default()
        };

        let config = crate::config::LintConfig::default();

        crate::compile_with(src, &options)
            .unwrap()
            .lints
            .iter()
            .map(|l| l.name)
            .filter(|n| crate::lint::level_of(&config, n) != crate::lint::Level::Allow)
            .collect()
    }

    #[test]
    fn a_long_parameter_list_fires_past_the_limit() {
        let src = "local function f(a, b, c, d, e, f, g, h) end\n";
        assert_eq!(names(src), vec!["too_many_arguments"]);
        assert_eq!(
            names_with(
                src,
                Thresholds {
                    too_many_arguments: 8,
                    ..Thresholds::default()
                }
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("function M:f(self, a, b, c, d, e, f, g) end\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_long_function_fires() {
        let body = "    print(1)\n".repeat(4);
        let src = format!("local function f()\n{body}end\n");
        assert_eq!(
            names_with(
                &src,
                Thresholds {
                    too_many_lines: 3,
                    ..Thresholds::default()
                }
            ),
            vec!["too_many_lines"]
        );
        assert_eq!(names(&src), Vec::<&str>::new());
    }

    #[test]
    fn deep_nesting_fires_once_at_the_limit() {
        let src = "local function f(a)\n    if a then\n        local x = 1\n        if a then\n            local y = 2\n            if a then\n                print(x, y)\n            end\n        end\n    end\nend\n";
        assert_eq!(
            names_with(
                src,
                Thresholds {
                    max_nesting: 3,
                    ..Thresholds::default()
                }
            ),
            vec!["deep_nesting"]
        );
    }

    #[test]
    fn branches_score_with_their_depth() {
        let src = "local function f(a, b)\n    if a then\n        for i = 1, 2 do\n            if b and a then\n                print(i)\n            end\n        end\n    end\nend\n";
        // if: 1, for: 2, if: 3, and: 1.
        assert_eq!(
            names_with(
                src,
                Thresholds {
                    cognitive_complexity: 6,
                    ..Thresholds::default()
                }
            ),
            vec!["cognitive_complexity"]
        );
        assert_eq!(
            names_with(
                src,
                Thresholds {
                    cognitive_complexity: 7,
                    ..Thresholds::default()
                }
            ),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn nested_ifs_collapse() {
        assert_eq!(
            fixed("if a then\n    if b or c then\n        print(1)\n    end\nend\n"),
            "if a and (b or c) then\n        print(1)\n    end\n"
        );
        assert_eq!(
            names("if a then\n    if b then\n        print(1)\n    end\n    print(2)\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn else_if_collapses_to_elseif() {
        assert_eq!(
            fixed(
                "if a then\n    print(1)\nelse\n    if b then\n        print(2)\n    else\n        print(3)\n    end\nend\n"
            ),
            "if a then\n    print(1)\nelseif b then\n        print(2)\n    else\n        print(3)\n    end\n"
        );
    }

    #[test]
    fn loop_work_fires() {
        assert_eq!(
            names("local s = \"\"\nfor i = 1, 3 do\n    s = s .. i\nend\n"),
            vec!["concat_in_loop"]
        );
        assert_eq!(
            names("for i = 1, 3 do\n    local rs = game:GetService(\"RunService\")\nend\n"),
            vec!["service_in_loop"]
        );
        assert_eq!(
            names("local rs = game:GetService(\"RunService\")\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn an_insert_at_the_length_appends() {
        assert_eq!(
            fixed("table.insert(t, #t + 1, v)\n"),
            "table.insert(t, v)\n"
        );
    }
}
