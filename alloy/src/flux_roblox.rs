//! Flux: the Roblox lints and the pedantic comment lints. A lowercase
//! method from the old API, a parent passed to `Instance.new`, a body
//! mover; a `TODO`, a stray `print`, an export with no comment. The
//! names and levels sit in `lint::LINTS`.

use crate::flux_scan::Scan;
use crate::lint::Lint;

/// Runs the Roblox and pedantic lints on one file.
pub(crate) fn run(s: &Scan) -> Vec<Lint> {
    let mut out = Vec::new();
    s.deprecated_method(&mut out);
    s.instance_new_parent(&mut out);
    s.deprecated_body_mover(&mut out);
    s.todo_comment(&mut out);
    s.print_debug(&mut out);
    s.missing_doc(&mut out);
    out
}

/// The lowercase members of the old API and their current names.
const DEPRECATED_METHODS: &[(&str, &str)] = &[
    ("connect", "Connect"),
    ("disconnect", "Disconnect"),
    ("wait", "Wait"),
    ("remove", "Destroy"),
    ("clone", "Clone"),
    ("children", "GetChildren"),
    ("getChildren", "GetChildren"),
    ("findFirstChild", "FindFirstChild"),
    ("findFirstChildOfClass", "FindFirstChildOfClass"),
    ("findFirstAncestor", "FindFirstAncestor"),
    ("isA", "IsA"),
    ("isDescendantOf", "IsDescendantOf"),
    ("isAncestorOf", "IsAncestorOf"),
    ("getService", "GetService"),
    ("service", "GetService"),
    ("getPlayers", "GetPlayers"),
    ("getMass", "GetMass"),
    ("breakJoints", "BreakJoints"),
    ("makeJoints", "MakeJoints"),
    ("loadAnimation", "LoadAnimation"),
    ("getPlayerFromCharacter", "GetPlayerFromCharacter"),
];

/// The body movers and what replaces each.
const BODY_MOVERS: &[(&str, &str)] = &[
    ("BodyVelocity", "LinearVelocity"),
    ("BodyPosition", "AlignPosition"),
    ("BodyGyro", "AlignOrientation"),
    ("BodyForce", "VectorForce"),
    ("BodyThrust", "VectorForce"),
    ("BodyAngularVelocity", "AngularVelocity"),
    ("RocketPropulsion", "LineForce with AlignOrientation"),
];

impl<'s> Scan<'s> {
    /// `:connect(`, `:wait(`, and the other lowercase members.
    fn deprecated_method(&self, out: &mut Vec<Lint>) {
        let declared = self.declared_functions();

        for i in 0..self.toks.len() {
            if !self.at(i, ":") || !self.at(i + 2, "(") {
                continue;
            }

            let name = self.t(i + 1);
            let Some((_, current)) = DEPRECATED_METHODS.iter().find(|(old, _)| *old == name) else {
                continue;
            };

            if declared.contains(&name) {
                continue;
            }

            self.lint(
                out,
                "deprecated_method",
                i + 1,
                i + 1,
                format!("`:{name}()` is the old name; `:{current}()` is the current one"),
                Some((*current).to_string()),
            );
        }
    }

    /// The `(` of `Instance.new` at `i`, when the call is one.
    fn instance_new_open(&self, i: usize) -> Option<usize> {
        (self.at(i, "Instance")
            && self.at(i + 1, ".")
            && self.at(i + 2, "new")
            && self.at(i + 3, "(")
            && !matches!(self.prev(i), "." | ":"))
        .then_some(i + 3)
    }

    /// `Instance.new(class, parent)`.
    fn instance_new_parent(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let Some(open) = self.instance_new_open(i) else {
                continue;
            };
            let Some(close) = self.matching(open) else {
                continue;
            };
            let commas = (open + 1..close)
                .filter(|j| self.at(*j, ",") && self.matching_depth(open, *j) == 1)
                .count();

            if commas == 0 {
                continue;
            }

            let class = self.string_content(open + 1).unwrap_or("class");
            self.lint(
                out,
                "instance_new_parent",
                i,
                close,
                format!(
                    "`Instance.new(\"{class}\", parent)` parents the instance before its properties are set; set `Parent` last"
                ),
                None,
            );
        }
    }

    /// `Instance.new("BodyVelocity")` and the other body movers.
    fn deprecated_body_mover(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let Some(open) = self.instance_new_open(i) else {
                continue;
            };
            let Some(class) = self.string_content(open + 1) else {
                continue;
            };
            let Some((_, replacement)) = BODY_MOVERS.iter().find(|(old, _)| *old == class) else {
                continue;
            };

            self.lint(
                out,
                "deprecated_body_mover",
                open + 1,
                open + 1,
                format!("`{class}` is deprecated; `{replacement}` on an `Attachment` replaces it"),
                None,
            );
        }
    }

    /// A `TODO`, `FIXME`, `XXX`, or `HACK` comment.
    fn todo_comment(&self, out: &mut Vec<Lint>) {
        for (start, end, text) in self.comments() {
            let Some(word) = ["TODO", "FIXME", "XXX", "HACK"]
                .iter()
                .find(|w| text.contains(*w))
            else {
                continue;
            };

            out.push(Lint {
                name: "todo_comment",
                start,
                end,
                message: format!("a `{word}` comment marks work that is not done"),
                fix: None,
            });
        }
    }

    /// A `print` call.
    fn print_debug(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if self.at(i, "print")
                && self.at(i + 1, "(")
                && !matches!(self.prev(i), "." | ":" | "function" | "local")
            {
                self.lint(
                    out,
                    "print_debug",
                    i,
                    i,
                    "a `print` writes to every player's output; remove it or route it through a logger"
                        .to_string(),
                    None,
                );
            }
        }
    }

    /// An `export` with no comment line right above it.
    fn missing_doc(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "export") || !self.statement_start(i) {
                continue;
            }

            // `export { a, b }` and `export default` are lists, not declarations.
            if matches!(self.t(i + 1), "{" | "default") {
                continue;
            }

            // The line above ends in a comment: documented.
            let gap = self.gap_before(i);
            let above = gap.trim_end_matches([' ', '\t']);
            let above = above.strip_suffix('\n').unwrap_or(above);
            let last_line = above.rsplit('\n').next().unwrap_or("");

            if last_line.trim_start().starts_with("--") {
                continue;
            }

            let mut j = i + 1;

            while j < self.toks.len() && !self.is_name(j) {
                j += 1;
            }

            let name = if self.is_name(j) { self.t(j) } else { "this" };
            self.lint(
                out,
                "missing_doc",
                i,
                j.min(self.toks.len() - 1),
                format!("`{name}` is exported and has no comment above it; say what it is for"),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::apply_fixes;

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

    #[test]
    fn old_method_names_take_the_new_ones() {
        assert_eq!(
            fixed("part.Touched:connect(f)\nlocal c = part:clone()\n"),
            "part.Touched:Connect(f)\nlocal c = part:Clone()\n"
        );
        assert_eq!(
            names("function Signal:connect(f) end\nlocal c = s:connect(f)\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_parent_argument_fires() {
        assert_eq!(
            names("local p = Instance.new(\"Part\", workspace)\n"),
            vec!["instance_new_parent"]
        );
        assert_eq!(
            names("local p = Instance.new(\"Part\")\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_body_mover_fires() {
        assert_eq!(
            names("local bv = Instance.new(\"BodyVelocity\")\n"),
            vec!["deprecated_body_mover"]
        );
    }

    /// Every lint, the pedantic ones included.
    fn all(src: &str) -> Vec<&'static str> {
        lints(src).iter().map(|l| l.name).collect()
    }

    #[test]
    fn the_pedantic_comment_lints_fire() {
        assert_eq!(all("-- TODO: later\nlocal x = 1\n"), vec!["todo_comment"]);
        assert_eq!(all("print(1)\n"), vec!["print_debug"]);
        assert_eq!(
            all("export function f(): number\n    return 1\nend\n"),
            vec!["missing_doc"]
        );
        assert_eq!(
            all("-- Adds one.\nexport function f(): number\n    return 1\nend\n"),
            Vec::<&str>::new()
        );
    }
}
