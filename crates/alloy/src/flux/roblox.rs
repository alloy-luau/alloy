//! Flux: the Roblox lints and the pedantic comment lints. A lowercase
//! method from the old API, a parent passed to `Instance.new`, a body
//! mover; a `TODO`, a stray `print`, an export with no comment. The
//! names and levels sit in `lint::LINTS`.

use super::scan::Scan;
use crate::lint::Lint;

/// Runs the Roblox and pedantic lints on one file.
pub(crate) fn run(s: &Scan) -> Vec<Lint> {
    let mut out = Vec::new();
    s.deprecated_method(&mut out);
    s.instance_new_parent(&mut out);
    s.deprecated_body_mover(&mut out);
    s.prefer_destroy(&mut out);
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

/// The two old names a std container also carries: `HashMap:remove(key)`
/// and a user `clone(self, ...)` both take arguments, and the Roblox
/// members take none. An empty call is the deprecated one.
const DEPRECATED_WITHOUT_ARGS: &[(&str, &str)] = &[("remove", "Destroy"), ("clone", "Clone")];

/// The old names the std spells the same way: `sig:connect(f)`,
/// `conn:disconnect()`, `sig:wait()`, `x:clone()` on a `T: Clone`, and
/// `bag:remove()`. Each fires only over a Roblox receiver, which
/// `roblox_expr` reads from the tokens.
const NEEDS_ROBLOX_RECEIVER: &[&str] = &["connect", "disconnect", "wait", "remove", "clone"];

/// The globals that name an instance with no member behind them.
const INSTANCE_GLOBALS: &[&str] = &["game", "workspace", "Workspace", "script"];

/// Whether an annotation names a Roblox event or the connection one
/// returns. The std's own `Signal` and `SignalConnection` do not.
fn names_event_type(ty: &str) -> bool {
    ty.contains("RBXScriptSignal") || ty.contains("RBXScriptConnection")
}

/// Whether an annotation names an Instance or one of its classes.
fn names_instance_type(ty: &str) -> bool {
    let base = ty.trim().trim_end_matches('?').trim();

    crate::roblox_classes::INSTANCE_CLASSES.contains(&base)
}

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
            let empty_call = self.at(i + 3, ")");
            let current = DEPRECATED_METHODS
                .iter()
                .find(|(old, _)| *old == name)
                .or_else(|| {
                    empty_call
                        .then(|| DEPRECATED_WITHOUT_ARGS.iter().find(|(old, _)| *old == name))
                        .flatten()
                });
            let Some((_, current)) = current else {
                continue;
            };

            if declared.contains(&name) {
                continue;
            }

            // `@derive(Clone)` writes `clone`, and a struct that has it
            // is not an Instance. The derive declares the method the
            // way a written one does.
            if name == "clone" && self.src.contains("@derive(") && self.derives("Clone") {
                continue;
            }

            // The std spells these names the way the old API does.
            // Over a receiver that is not a Roblox instance or event,
            // the call is the std's, not the 2014 member.
            if NEEDS_ROBLOX_RECEIVER.contains(&name) {
                let event = matches!(name, "connect" | "disconnect" | "wait");

                if !self.roblox_expr(self.chain_start(i), i, event, true) {
                    continue;
                }
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

    /// The head of the call chain that ends at `at`: names, `.`, `:`,
    /// and whole bracket groups, walked back. `expr_start_before` stops
    /// inside `f("x")`, and the name before that call is the one that
    /// says whether the receiver is a Roblox value.
    fn chain_start(&self, at: usize) -> usize {
        let mut c = at;

        while c > 0 {
            // A chain never crosses a statement boundary. `workspace:wait()`
            // on the line above is its own statement, so the walk stops
            // there and each call answers for its own receiver.
            if c != at && self.statement_start(c) {
                break;
            }

            let j = c - 1;
            let text = self.t(j);

            if matches!(text, ")" | "]" | "}") {
                match self.opener(j) {
                    Some(open) => c = open,
                    None => break,
                }

                continue;
            }

            if matches!(text, "." | ":") || self.is_name(j) {
                c = j;

                continue;
            }

            break;
        }

        c
    }

    /// The bracket that opens the group closing at `close`.
    fn opener(&self, close: usize) -> Option<usize> {
        let mut depth = 0i32;

        for j in (0..=close).rev() {
            let text = self.t(j);

            if matches!(text, ")" | "]" | "}") {
                depth += 1;
            } else if matches!(text, "(" | "[" | "{") || text.ends_with('(') || text.ends_with('[')
            {
                depth -= 1;

                if depth == 0 {
                    return Some(j);
                }
            }
        }

        None
    }

    /// Whether the tokens `a..b` read a Roblox instance, or an event on
    /// one when `event` is set. Three shapes say yes: a member that
    /// starts with a capital, since every std member is snake case; one
    /// of the instance globals; and a name the file annotates with a
    /// Roblox class. `deep` allows one step back to a local's
    /// initializer, so `local part = workspace.Part` carries over.
    fn roblox_expr(&self, a: usize, b: usize, event: bool, deep: bool) -> bool {
        // A chain that starts at an imported name reaches another module,
        // `Shop.Offer.default()`, not a child in the DataModel.
        if self.is_name(a) && self.imported(self.t(a)) {
            return false;
        }

        for j in a..b {
            if !self.is_name(j) || !self.t(j).starts_with(|c: char| c.is_ascii_uppercase()) {
                continue;
            }

            if matches!(self.prev(j), "." | ":") {
                return true;
            }
        }

        // An event needs a member to hang on; a bare `workspace` is an
        // instance, and `workspace:clone()` is the old `Clone`.
        if !event && a + 1 == b && INSTANCE_GLOBALS.contains(&self.t(a)) {
            return true;
        }

        // `props.part:clone()`: the receiver ends in a field, and the
        // field's own annotation says what it holds.
        if b >= a + 3
            && matches!(self.t(b - 2), "." | "?.")
            && self.is_name(b - 1)
            && let Some(ty) = self.field_type(self.t(b - 1))
        {
            return if event {
                names_event_type(ty)
            } else {
                names_instance_type(ty)
            };
        }

        if a + 1 != b || !self.is_name(a) {
            return false;
        }

        // The binding the name reads at `a` answers. An earlier `conn`
        // in a closed block is another value.
        if let Some(ty) = self.type_at(a) {
            return if event {
                names_event_type(ty)
            } else {
                names_instance_type(ty)
            };
        }

        deep && match self.init_at(a) {
            Some((s, e)) => self.roblox_expr(s, e, event, false),
            None => false,
        }
    }

    /// The type a field declares: `part: Instance` in a table type, a
    /// struct body, or an interface. A field of a parameter is the
    /// receiver of a call, and the field's own annotation says what it
    /// holds. The first declaration of the name answers, the way
    /// `declared_type` answers for a binding.
    fn field_type(&self, name: &str) -> Option<&'s str> {
        for j in 0..self.toks.len() {
            if !self.is_name(j) || self.t(j) != name || !self.at(j + 1, ":") {
                continue;
            }

            // A field sits after `{` or a comma in a table type, and on
            // its own line in a struct or an interface body.
            if !matches!(self.prev(j), "{" | "," | "read" | "write") && !self.statement_start(j) {
                continue;
            }

            let ty = j + 2;

            // `t:method(...)` reads as an annotation from the tokens
            // alone; the `(` after the name says it is a call.
            if self.is_name(ty) && !self.at(ty + 1, "(") {
                return Some(self.t(ty));
            }
        }

        None
    }

    /// Whether an `import` of the file binds `name` from a module: between
    /// `import` and `from`, as a listed name, an alias, or the module
    /// name. A service from `@game` is an instance, and does not count.
    fn imported(&self, name: &str) -> bool {
        (0..self.toks.len())
            .filter(|&i| self.at(i, "import") && self.statement_start(i))
            .any(|i| {
                let mut j = i + 1;
                let mut binds = false;

                while j < self.toks.len() && !self.at(j, "from") && !self.statement_start(j) {
                    binds |= self.t(j) == name;
                    j += 1;
                }

                let spec = self.toks.get(j + 1).map_or("", |_| self.t(j + 1));

                binds && !spec[1.min(spec.len())..].starts_with("@game")
            })
    }

    /// The tokens of the value the name at `at` holds: its `local` in
    /// scope says, or the first `local` of the name in the file when no
    /// local or parameter in scope binds it.
    fn init_at(&self, at: usize) -> Option<(usize, usize)> {
        match self.binding_at(at) {
            Some(d) if self.at(d.wrapping_sub(1), "local") => self.local_value(d),

            Some(_) => None,

            None => (1..self.toks.len())
                .filter(|&j| self.at(j - 1, "local") && self.is_name(j) && self.t(j) == self.t(at))
                .find_map(|j| self.local_value(j)),
        }
    }

    /// The tokens of the value in `local name = value`, on one line, for
    /// the name at `j`.
    fn local_value(&self, j: usize) -> Option<(usize, usize)> {
        if !self.at(j + 1, "=") {
            return None;
        }

        let line = self.line_of(j);
        let mut k = j + 2;

        while k < self.toks.len() && self.line_of(k) == line {
            k += 1;
        }

        (k > j + 2).then_some((j + 2, k))
    }

    /// Whether the file names a derive, as in `@derive(Debug, Clone)`.
    fn derives(&self, wanted: &str) -> bool {
        for i in 0..self.toks.len() {
            if !(self.at(i, "@") && self.at(i + 1, "derive") && self.at(i + 2, "(")) {
                continue;
            }

            let Some(close) = self.matching(i + 2) else {
                continue;
            };

            if (i + 3..close).any(|j| self.at(j, wanted)) {
                return true;
            }
        }

        false
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
    /// Whether the name at `i` is an Instance the file says is one: an
    /// annotation with a class name, or a `local x = Instance.new(...)`.
    fn plain_instance(&self, i: usize) -> bool {
        if !self.is_name(i) || matches!(self.prev(i), "." | ":") {
            return false;
        }

        if self.type_at(i).is_some_and(names_instance_type) {
            return true;
        }

        match self.init_at(i) {
            Some((a, _)) => {
                self.instance_new_open(a).is_some()
                    || (self.at(a, "new") && self.at(a + 1, "Instance"))
            }

            None => false,
        }
    }

    /// Whether a scope holds the name, or holds an item that names it
    /// as its owner: `scope:add(part)` and `scope:add(conn, part)`.
    fn scope_holds(&self, name: &str) -> bool {
        for i in 0..self.toks.len() {
            if !(self.at(i, ":") && self.at(i + 1, "add") && self.at(i + 2, "(")) {
                continue;
            }

            let Some(close) = self.matching(i + 2) else {
                continue;
            };

            if (i + 3..close)
                .any(|j| self.t(j) == name && self.is_name(j) && !matches!(self.prev(j), "." | ":"))
            {
                return true;
            }
        }

        false
    }

    /// `delete part` where the file shows `part` is a plain Instance.
    fn prefer_destroy(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "delete") || !self.statement_start(i) || !self.plain_instance(i + 1) {
                continue;
            }

            // The name is the whole operand. `delete t.part` empties the
            // slot as well, which `destroy` does not do.
            if self.statement_end(i) != i + 2 {
                continue;
            }

            let name = self.t(i + 1);

            if self.scope_holds(name) {
                continue;
            }

            self.lint(
                out,
                "prefer_destroy",
                i,
                i,
                format!(
                    "`{name}` is an Instance with nothing else to clean; `destroy {name}` says what happens"
                ),
                Some("destroy".to_string()),
            );
        }
    }

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

    /// An `export` with no comment line right above it. A `type` alias
    /// and an `interface` report under `missing_doc_type`, the rest
    /// under `missing_doc`.
    fn missing_doc(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.at(i, "export") || self.at(i, "global")) || !self.statement_start(i) {
                continue;
            }

            // `export { a, b }` and `export default` are lists, not declarations.
            if matches!(self.t(i + 1), "{" | "default") {
                continue;
            }

            // `export` on a namespace member exports nothing; the build
            // reports the word, so the member is no export to document.
            if self.in_namespace(i) {
                continue;
            }

            // The attributes above the export belong to it, so the
            // comment sits above them: `@timeout(5)` under the doc.
            let mut top = i;

            loop {
                let name_at = match top.checked_sub(1).map(|k| self.t(k)) {
                    Some(")") => self.opener(top - 1).and_then(|o| o.checked_sub(1)),

                    Some(_) => Some(top - 1),

                    None => None,
                };
                // `@M.tag` and `@serde.rename_all(...)` name a path.
                let head = name_at.filter(|n| self.is_name(*n)).map(|mut n| {
                    while n >= 2 && self.t(n - 1) == "." && self.is_name(n - 2) {
                        n -= 2;
                    }

                    n
                });

                match head {
                    Some(n) if n > 0 && self.t(n - 1) == "@" => top = n - 1,

                    _ => break,
                }
            }

            // The line above ends in a comment: documented.
            let gap = self.gap_before(top);
            let above = gap.trim_end_matches([' ', '\t']);
            let above = above.strip_suffix('\n').unwrap_or(above);
            let last_line = above.rsplit('\n').next().unwrap_or("");

            if last_line.trim_start().starts_with("--") {
                continue;
            }

            // The keywords between `export` and the name are not it:
            // `export async function load`, `export const LIMIT`.
            let mut j = i + 1;

            while j < self.toks.len()
                && (!self.is_name(j)
                    || matches!(
                        self.t(j),
                        "async"
                            | "function"
                            | "const"
                            | "local"
                            | "struct"
                            | "enum"
                            | "type"
                            | "interface"
                            | "trait"
                            | "remote"
                            | "attribute"
                            | "macro"
                            | "impl"
                            | "class"
                            | "namespace"
                    ))
            {
                j += 1;
            }

            // A `type` alias and an `interface` say what they are in
            // their own shape, so they answer to `missing_doc_type`,
            // which no mode turns on.
            let shapes = (i + 1..=j).any(|k| matches!(self.t(k), "type" | "interface"));
            let name = if self.is_name(j) { self.t(j) } else { "this" };
            self.lint(
                out,
                if shapes {
                    "missing_doc_type"
                } else {
                    "missing_doc"
                },
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
    use super::super::helpers::{fixed_by, lints as lints_of, names_of};

    /// The sources here bind names to show a shape, not to read them.
    fn lints(src: &str) -> Vec<crate::Lint> {
        lints_of(src, &["unused_variable", "redundant_as", "prefer_const"])
    }

    fn fixed(src: &str) -> String {
        fixed_by(src, &lints(src))
    }

    fn names(src: &str) -> Vec<&'static str> {
        names_of(&lints(src))
    }

    /// The lint names of a source, pedantic ones included.
    fn all_names(src: &str) -> Vec<&'static str> {
        lints(src).iter().map(|l| l.name).collect()
    }

    /// `delete` on a plain Instance says less than `destroy`. A path, a
    /// value the file says nothing about, and an Instance a scope holds
    /// something for all stand down.
    #[test]
    fn a_plain_instance_takes_destroy() {
        assert_eq!(
            all_names("local part = Instance.new(\"Part\")\ndelete part\n"),
            vec!["prefer_destroy"]
        );
        assert_eq!(
            all_names("local part: Part = workspace.Ball\ndelete part\n"),
            vec!["prefer_destroy"]
        );
        assert_eq!(
            fixed("local part = new Instance(\"Part\")\ndelete part\n"),
            "local part = new Instance(\"Part\")\ndestroy part\n"
        );

        // The lint is off unless the config asks for it.
        assert_eq!(
            names("local part = Instance.new(\"Part\")\ndelete part\n"),
            Vec::<&str>::new()
        );

        // A scope holds something for the Instance, so `delete` has
        // work to do.
        assert!(!all_names(
            "local part = Instance.new(\"Part\")\nlocal scope = Scope.new()\nscope:add(part.Touched:Connect(f), part)\ndelete part\n"
        )
        .contains(&"prefer_destroy"));

        // A field empties its slot, and a value of no named type is not
        // an Instance as far as the file says.
        assert!(!all_names("delete self.part\n").contains(&"prefer_destroy"));
        assert!(!all_names("local bag = make()\ndelete bag\n").contains(&"prefer_destroy"));
    }

    #[test]
    fn old_method_names_take_the_new_ones() {
        assert_eq!(
            fixed("part.Touched:connect(f)\nlocal c = workspace.Ball:clone()\n"),
            "part.Touched:Connect(f)\nlocal c = workspace.Ball:Clone()\n"
        );
        assert_eq!(
            names("function Signal:connect(f) end\nlocal c = s:connect(f)\n"),
            Vec::<&str>::new()
        );
    }

    /// The receiver decides. An event on an instance, a signal the file
    /// annotates, and a `GetPropertyChangedSignal` call all take the
    /// current name.
    #[test]
    fn a_roblox_receiver_fires() {
        assert_eq!(
            names("game.Players.PlayerAdded:connect(f)\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names("humanoid:GetPropertyChangedSignal(\"Health\"):connect(f)\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names("local touched: RBXScriptSignal = part.Touched\ntouched:connect(f)\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names("local conn: RBXScriptConnection = part.Touched:Connect(f)\nconn:disconnect()\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names("local part: Part = workspace.Ball\npart:remove()\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(names("script.Parent:clone()\n"), vec!["deprecated_method"]);
        // A field of a parameter: the field's own annotation says what
        // the receiver holds.
        assert_eq!(
            names(
                "function _g(props: { part: Instance })\n    local n = props.part:clone()\n    print(n)\nend\n"
            ),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names(
                "struct Props as\n    part: Instance\nend\n\nfunction _g(p: Props)\n    local n = p.part:clone()\n    print(n)\nend\n"
            ),
            vec!["deprecated_method"]
        );
        // A field of no Roblox type keeps the std's spelling.
        assert_eq!(
            names(
                "function _g(props: { part: Widget })\n    local n = props.part:clone()\n    print(n)\nend\n"
            ),
            Vec::<&str>::new()
        );
        // A `:wait()` on the line above is its own statement. Each call
        // answers for its own receiver.
        assert_eq!(
            names("workspace:wait()\nworkspace:clone()\n"),
            vec!["deprecated_method"]
        );
        assert_eq!(
            names("workspace:wait()\nworkspace:remove()\nscript:remove()\n"),
            vec!["deprecated_method"; 2]
        );
    }

    /// The std spells `connect`, `disconnect`, `wait`, and `clone` the
    /// way the 2014 API did. Over a value of the std, or a plain local,
    /// the lint stands down.
    #[test]
    fn a_std_receiver_stands_down() {
        assert_eq!(
            names(
                "local sig: Signal<number> = Signal.new()\nlocal conn = sig:connect(f)\nconn:disconnect()\nsig:wait()\n"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("function copy<T: Clone>(x: T): T\n    return x:clone()\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local s = make()\ns:connect(f)\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local c: SignalConnection = sig:connect(f)\nc:disconnect()\n"),
            Vec::<&str>::new()
        );
    }

    /// The receiver reads the binding in scope. A `conn` of a Roblox
    /// event in a closed block is another value than the `conn` of a
    /// std signal after it, and a parameter holds its own function.
    #[test]
    fn a_receiver_reads_the_binding_in_scope() {
        let src = "do\n    local conn = damaged:Connect(f)\n    conn:Disconnect()\nend\ndo\n    local conn = damaged:connect(f)\n    conn:disconnect()\nend\n";
        assert_eq!(names(src), Vec::<&str>::new());

        let roblox = "local conn = sig:connect(f)\ndo\n    local conn = part.Touched:Connect(f)\n    conn:disconnect()\nend\nconn:disconnect()\n";
        let got = lints(roblox);
        assert_eq!(names_of(&got), vec!["deprecated_method"]);
        assert_eq!(got[0].start, roblox.find("disconnect").unwrap() as u32);

        let param = "local conn = part.Touched:Connect(f)\nlocal function g(conn: SignalConnection)\n    conn:disconnect()\nend\nprint(g)\n";
        assert_eq!(names(param), Vec::<&str>::new());
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

    /// `:clone()` and `:remove()` are the other two names the lint's
    /// own description gives. A `HashMap` has a `remove` of its own, so
    /// only a call with no arguments is the Roblox member.
    #[test]
    fn the_argument_free_old_names_fire() {
        assert_eq!(
            fixed("local c = workspace.Ball:clone()\nscript:remove()\n"),
            "local c = workspace.Ball:Clone()\nscript:Destroy()\n"
        );
        assert_eq!(names("local v = bag:remove(\"key\")\n"), Vec::<&str>::new());
        assert_eq!(names("local c = t:clone(1)\n"), Vec::<&str>::new());
    }

    /// The doc sits above the attributes, and an attribute through a
    /// path, `@M.tag`, is one of them. The walk stopped at the path, so a
    /// documented export reported.
    #[test]
    fn a_doc_sits_above_a_dotted_attribute() {
        for attr in ["@M.tag", "@serde.rename_all(\"camelCase\")", "@A.B.tag(1)"] {
            let src = format!("-- Doc.\n{attr}\nexport struct E as\n    x: number\nend\n");
            assert_eq!(all(&src), Vec::<&str>::new(), "{src}");

            let bare = format!("{attr}\nexport struct E as\n    x: number\nend\n");
            assert_eq!(all(&bare), vec!["missing_doc"], "{bare}");
        }
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
        // A type alias and an interface say what they are in their own
        // shape, so they report under a name of their own, which
        // `strict` leaves off. The other exports stay with `missing_doc`.
        assert_eq!(
            all("export type Label = string\n"),
            vec!["missing_doc_type"]
        );
        assert_eq!(
            all("export interface Shape as\n    area: number\nend\n"),
            vec!["missing_doc_type"]
        );
        assert_eq!(
            all("export struct Box as\n    width: number\nend\n"),
            vec!["missing_doc"]
        );
        assert_eq!(all("export const N = 1\n"), vec!["missing_doc"]);
        assert_eq!(
            all("-- Asks.\n@timeout(5)\n@unreliable\nexport remote Ping() from client\n"),
            Vec::<&str>::new()
        );

        // `export` on a namespace member exports nothing, and the build
        // says so; the lint does not call the member exported.
        assert_eq!(
            all(
                "-- The group.\nexport namespace Outer as\n    export namespace Inner as\n        public function greet(): string\n            return \"hi\"\n        end\n    end\nend\n"
            ),
            Vec::<&str>::new()
        );
    }
}
