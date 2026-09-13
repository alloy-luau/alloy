//! The message rewrites: turning a raw luau-lsp diagnostic into words
//! the Alloy source earns. `mod.rs` runs the analyzer and calls into
//! this module for every report it maps back onto a source.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::TypeDiag;

/// The book section a report kind belongs to; `None` leaves the report
/// to the checker's own `luau` code.
pub fn section_of(kind: &str) -> Option<&'static str> {
    match kind {
        "UnknownModule" => Some("3.2"),
        "DirectiveError" => Some("4.4"),
        "StructError" => Some("3.6"),
        "EnumError" => Some("3.4"),
        "ExhaustiveMatch" => Some("4.2"),
        _ => None,
    }
}

/// The runtime's own names taken out of a printed type: the require
/// binding, the mapped-type functions, and the `__all` suffix an
/// exported table carries.
pub fn strip_std_prefix(text: &str) -> String {
    if !(text.contains("__alloy") || text.contains("__mapped_") || text.contains("__all")) {
        return text.to_string();
    }

    let mut out = text.to_string();

    for primitive in crate::desugar::PRIMITIVES {
        out = out.replace(&format!("__alloy_{primitive}."), &format!("{primitive}."));
    }

    // `__all` is the suffix an exported table carries. Stripping it
    // inside `__alloy` would leave `oy`, so the name has to end there.
    let mut cut = out.replace("__alloy.", "");
    let mut from = 0;

    while let Some(i) = cut[from..].find("__all") {
        let at = from + i;
        let end = at + "__all".len();

        if cut[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            from = end;

            continue;
        }

        cut.replace_range(at..end, "");
        from = at;
    }

    cut.replace("__mapped_optional<", "Partial<")
        .replace("__mapped_read<", "Readonly<")
        .replace("__mapped_write<", "Sink<")
}

/// A checker message as a reader of the source should see it: the
/// runtime's names go, a struct's private view folds to the struct,
/// `Array<T>` reads `T[]`, and the tail that walks the emitted shape is
/// cut. The language server runs the same pass, so the terminal and the
/// editor say one thing.
pub fn friendly_type_message(
    message: &str,
    known: &crate::shapes::Known,
    line: Option<&str>,
    col: usize,
) -> String {
    // The shared pass writes a kind of its own; the caller prints the
    // report's kind, so one of the two goes.
    let cut = crate::shapes::friendly_text(message);
    let cut = cut
        .split_once(": ")
        .filter(|(kind, _)| kind.ends_with("Error") && !kind.contains(' '))
        .map_or(cut.as_str(), |(_, rest)| rest);
    let stripped = strip_std_prefix(cut);
    let folded = drop_result_methods(&crate::shapes::fold(&stripped, known));

    if let Some(hint) = crate::shapes::plain_table_hint(&folded) {
        return hint;
    }

    // The fold names an Array after the shared pass ran, so the two
    // spellings only meet here.
    let folded = crate::shapes::table_beside_array(&folded).unwrap_or(folded);

    let Some(line) = line else {
        return folded;
    };

    // The remote rewrite reads the surface the checker printed, so it
    // runs on the text before the fold names it as well as after.
    if let Some(better) = rewrite_remote_key(&stripped, line)
        .or_else(|| rewrite_remote_key(&folded, line))
        .or_else(|| rewrite_await(&folded, line))
        .or_else(|| rewrite_arity(&folded, line, col))
        .or_else(|| rewrite_dot_self(&folded, line, col))
    {
        return better;
    }

    match constructor_field(line, col) {
        Some((field, name)) => format!("field `{field}` of `{name}`: {folded}"),

        None => folded,
    }
}

/// The one sentence for a `.` where a `:` belongs, and for the arity of
/// a method call the source writes without `self`. The CLI and the
/// editor both call it, so both say the same thing.
///
/// `line` is the source line the report sits on. `col` is one-based.
pub fn rewrite_dot_call(message: &str, line: &str, col: usize) -> Option<String> {
    rewrite_arity(message, line, col).or_else(|| rewrite_dot_self(message, line, col))
}

/// The phrase that names a `.` where a `:` belongs. A report on a line
/// that already carries it is the same mistake told again.
pub const DOT_FOR_COLON: &str = "` is a method; call it with `";

/// The checker's step limit, as `friendly_text` words it. Its other
/// reports on that line describe a solve it did not finish.
pub const SOLVER_LIMIT: &str = "the checker reached its limit";

/// The checker's lints Alloy replaces outright: `unused_variable`,
/// `unused_function`, and `unused_import` cover the same ground, in the
/// words of what the source wrote.
pub fn owned_lint(kind: &str) -> bool {
    matches!(kind, "LocalUnused" | "FunctionUnused" | "ImportUnused")
}

/// The Alloy lints that say what one of the checker's lints says. Alloy
/// names the construct the source wrote, so where both fire on a line
/// the checker's copy goes.
pub fn paired_lint(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "LocalUnused" => &["unused_variable"],

        "FunctionUnused" => &["unused_function"],

        "ImportUnused" => &["unused_import"],

        "TableLiteral" => &["duplicate_key"],

        "DuplicateFunction" => &["duplicate_function"],

        "ComparisonPrecedence" => &["misplaced_not", "bool_comparison"],

        "DeprecatedApi" => &["deprecated_global", "deprecated_method"],

        "TableOperations" => &["table_insert_position", "manual_push"],

        "UnreachableCode" => &["unreachable_code"],

        _ => return None,
    })
}

/// A report about a name the emit writes and the source does not, in
/// the source's words: `new Plain { }` and `x is Plain` on a type
/// alias, `impl T for Alias`, and `new n { }` on a value. The second
/// half of the answer is the line to move the report to; the `impl`
/// case reports once on the `impl` line rather than once per method.
/// The language server writes the same sentences.
pub fn rewrite_emitted_name(
    message: &str,
    source: &str,
    line: usize,
) -> Option<(String, Option<usize>)> {
    let text = source.lines().nth(line.saturating_sub(1))?;

    if let Some(name) = quoted_after(message, "Unknown global '") {
        if names_word(text, &format!("new {name}")) {
            return Some((format!("`{name}` is a type, not a struct"), None));
        }

        if names_word(text, &format!("is {name}")) {
            return Some((format!("`{name}` is not a type in scope"), None));
        }

        // Every method body of `impl T for Alias` reports the same
        // global; the `impl` line is where the mistake is.
        if let Some(at) = impl_line_for(source, line, name) {
            return Some((
                format!("`{name}` is a type, not a struct; `impl` needs one"),
                Some(at),
            ));
        }

        // A `type`, an `interface`, or a `trait` binds no value, so the
        // emit passes the name through and the checker looks for a
        // global of that name.
        if declares_type_only(source, name) {
            return Some((format!("`{name}` is a type, not a value"), None));
        }
    }

    // `new n { }`, where `n` is a value: the emit asks it for `new`.
    if let Some(owner) = quoted_after(message, "Type '")
        && message.ends_with("does not have key 'new'")
        && let Some(name) = word_after(text, "new ")
    {
        return Some((format!("`new` needs a struct; `{name}` is a {owner}"), None));
    }

    None
}

/// The text between `opener` and the next quote.
fn quoted_after<'a>(message: &'a str, opener: &str) -> Option<&'a str> {
    let at = message.find(opener)? + opener.len();

    message[at..].find('\'').map(|end| &message[at..at + end])
}

/// The identifier right after `opener` on a line.
fn word_after(line: &str, opener: &str) -> Option<String> {
    let at = line.find(opener)? + opener.len();
    let name: String = line[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    (!name.is_empty()).then_some(name)
}

/// Whether the line holds the phrase as whole words.
fn names_word(line: &str, phrase: &str) -> bool {
    line.match_indices(phrase).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + phrase.len()..].chars().next();

        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Whether the source declares the name as a type alone: a `type`, an
/// `interface`, or a `trait`. A struct and an enum bind a value too.
fn declares_type_only(source: &str, name: &str) -> bool {
    source.lines().any(|l| {
        let text = l.trim_start();
        let text = text.strip_prefix("export ").unwrap_or(text);

        ["type ", "interface ", "trait "].iter().any(|head| {
            text.strip_prefix(head).is_some_and(|rest| {
                rest.strip_prefix(name).is_some_and(|tail| {
                    !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            })
        })
    })
}

/// The one-based line of the `impl ... for Name` above a line, when one
/// opens the block the line sits in.
fn impl_line_for(source: &str, line: usize, name: &str) -> Option<usize> {
    source
        .lines()
        .take(line.saturating_sub(1))
        .enumerate()
        .filter(|(_, l)| {
            // `impl T for Alias as`: the header may close with `as`.
            let head = l.trim_end();
            let head = head.strip_suffix(" as").unwrap_or(head);

            l.trim_start().starts_with("impl ") && head.ends_with(&format!(" for {name}"))
        })
        .map(|(k, _)| k + 1)
        .last()
}

/// The std holds a Result's methods in an alias of their own, so the
/// checker prints `ResultMethods<T, E> & Result<T, E>`. The methods are
/// part of what `Result` is; the name for them is not the reader's.
fn drop_result_methods(text: &str) -> String {
    let mut out = text.to_string();

    while let Some(at) = out.find("ResultMethods") {
        let rest = &out[at..];
        let Some(open) = rest.find('<') else {
            break;
        };
        let Some(close) = rest[open..].find("> & ").map(|i| open + i + "> & ".len()) else {
            break;
        };

        out.replace_range(at..at + close, "");
    }

    out
}

/// The call that starts at a column: `:` or `.`, the receiver as the
/// source writes it, and the name after the separator.
fn call_head(line: &str, col: usize) -> Option<(char, String, String)> {
    let start = col.saturating_sub(1);
    let rest = line.get(start..)?;
    let head = &rest[..rest.find('(')?];
    let sep = head.rfind([':', '.'])?;
    let member = head[sep + 1..].trim();
    let name = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !name(member) {
        return None;
    }

    let receiver = head[..sep].trim();

    (!receiver.is_empty()).then(|| {
        (
            head.as_bytes()[sep] as char,
            receiver.to_string(),
            member.to_string(),
        )
    })
}

/// The counts of an argument-count message: what the function takes and
/// what the call passed. A range, `1 to 2`, gives its lower bound.
fn arity_counts(message: &str) -> Option<(usize, usize)> {
    let after = message.split_once("expects ")?.1;
    let expects: usize = after
        .split_whitespace()
        .next()?
        .parse()
        .ok()
        .filter(|n| *n > 0)?;
    let rest = message.split_once(", but ")?.1;
    let word = rest.trim_start_matches("only ").split_whitespace().next()?;
    let given = if word == "none" {
        0
    } else {
        word.parse().ok()?
    };

    Some((expects, given))
}

/// The argument-count message the source earns. A `:` call passes the
/// receiver as the first argument, which the reader did not write, so
/// both counts lose it. A `.` call of a method is one argument short
/// for that same reason, and that mistake reads better named.
fn rewrite_arity(message: &str, line: &str, col: usize) -> Option<String> {
    if !message.contains("Function expects") {
        return None;
    }

    let (expects, given) = arity_counts(message)?;
    let (sep, receiver, member) = call_head(line, col)?;

    if sep == '.' {
        // `expects 1 to 2 arguments` is a range: the call is short of
        // the lower bound, which says nothing about the separator.
        let ranged = message.contains(" to ");
        // A capitalized receiver names a module, a type, or a remote,
        // and each of those takes its `.`.
        let value = receiver.starts_with(|c: char| c.is_lowercase() || c == '_');

        return (!ranged && value && given + 1 == expects).then(|| {
            format!(
                "`{member}` is a method; call it with `{receiver}:{member}(...)`, not `{receiver}.{member}(...)`"
            )
        });
    }

    if given == 0 {
        return None;
    }

    let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
    let (expects, given) = (expects - 1, given - 1);
    let tail = if given < expects {
        format!(
            "but only {given} {} specified",
            if given == 1 { "is" } else { "are" }
        )
    } else {
        format!(
            "but {given} {} specified",
            if given == 1 { "is" } else { "are" }
        )
    };

    Some(format!(
        "Argument count mismatch. `{member}` takes {expects} {}, {tail}",
        plural(expects)
    ))
}

/// A `.` call of a method sends the first argument where the receiver
/// belongs, so the checker reports the mismatch against the method's
/// self parameter. The std writes that parameter `read T`, a type the
/// source never spells; the separator is the mistake, and it reads as
/// the arity rewrite says it.
fn rewrite_dot_self(message: &str, line: &str, col: usize) -> Option<String> {
    if !message.starts_with("Expected this to be 'read ") {
        return None;
    }

    let (sep, receiver, member) = enclosing_call(line, col)?;

    // A capitalized receiver names a module, a type, or a remote, and
    // each of those takes its `.`.
    (sep == '.' && receiver.starts_with(|c: char| c.is_lowercase() || c == '_')).then(|| {
        format!(
            "`{member}` is a method; call it with `{receiver}:{member}(...)`, not `{receiver}.{member}(...)`"
        )
    })
}

/// The call whose arguments hold a column: the separator, the receiver,
/// and the member. `call_head` reads a call that starts at the column;
/// this one reads the call the column sits inside.
fn enclosing_call(line: &str, col: usize) -> Option<(char, String, String)> {
    let upto = line.get(..col.saturating_sub(1))?;
    let open = upto.rfind('(')?;
    let head = &upto[..open];
    let sep = head.rfind([':', '.'])?;
    let member = head[sep + 1..].trim();
    let name = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !name(member) {
        return None;
    }

    let receiver: String = head[..sep]
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();

    (!receiver.is_empty()).then(|| (head.as_bytes()[sep] as char, receiver, member.to_string()))
}

/// `await` on a value that is no Future prints the std's own parameter,
/// `Settled<any>`, whose `__value` is a key of the type and not of the
/// source. Older spellings of the parameter still reach here.
fn rewrite_await(message: &str, line: &str) -> Option<String> {
    let bound = message.strip_prefix('`').and_then(|rest| {
        let (got, tail) = rest.split_once('`')?;

        tail.contains("the bound `Settled<").then_some(got)
    });
    let wanted = bound.is_some()
        || message.contains("'Awaitable<T>'")
        || message.contains("'Future<T>'")
        || message.contains("'Settled<");

    if !(wanted && line.contains("await ")) {
        return None;
    }

    let got = match bound {
        Some(got) => got,

        None => {
            let rest = message.split_once("but got '")?.1;

            rest.split('\'').next()?
        }
    };
    // A narrowed primitive prints as `typeof(string)`; the reader wrote
    // a string.
    let got = got
        .strip_prefix("typeof(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(got);

    Some(format!("`await` needs a Future; `{got}` is not one"))
}

/// A remote's whole surface reaches a missing-member message. The
/// reader knows it by the name they declared.
fn rewrite_remote_key(message: &str, line: &str) -> Option<String> {
    let key = message.strip_prefix("Key '")?.split('\'').next()?;
    let table = message.split_once("' not found in table '")?.1;
    // Every side of a remote carries `instance` and at least one of the
    // verbs; the fold may have named the whole surface already.
    let surface = table.contains("instance: Instance?")
        && ["on:", "fire", "call:", "wait:"]
            .iter()
            .any(|verb| table.contains(verb));

    if !(table.starts_with("Remote'") || surface) {
        return None;
    }

    // `Chat.call(...)` is the form the docs write, and `Chat:call(...)`
    // is the mistake a reader makes; both name the same member.
    let at = line
        .find(&format!(".{key}"))
        .or_else(|| line.find(&format!(":{key}")))?;
    let receiver: String = line[..at]
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if receiver.is_empty() {
        return None;
    }

    let members: Vec<&str> = table
        .trim_start_matches('{')
        .split(',')
        .filter_map(|part| part.split_once(':').map(|(k, _)| k.trim()))
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .collect();
    let near = members
        .iter()
        .map(|m| (edit_distance(m, key), *m))
        .filter(|(d, _)| *d <= 2)
        .min();

    Some(match near {
        Some((_, m)) => format!("remote `{receiver}` has no `{key}`; did you mean `{m}`?"),

        None => format!("remote `{receiver}` has no `{key}`"),
    })
}

/// The edit distance of two names, for a "did you mean".
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }

    row[b.len()]
}

/// The constructor field a column falls in: `new Plain { a = "x" }` at
/// the column of `"x"` gives `("a", "Plain")`. The checker reports the
/// value alone, and the reader wants to know which field it was for.
fn constructor_field(line: &str, col: usize) -> Option<(String, String)> {
    let at = line.find("new ")?;
    let rest = &line[at + 4..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() {
        return None;
    }

    let open = at + 4 + rest.find('{')?;
    let target = col.checked_sub(1)?;

    if target <= open {
        return None;
    }

    let mut depth = 0i32;
    let mut field: Option<String> = None;
    let mut key_start = open + 1;

    for (i, c) in line.char_indices().skip(open) {
        match c {
            '{' | '(' | '[' => depth += 1,

            '}' | ')' | ']' => {
                depth -= 1;

                if depth == 0 {
                    break;
                }
            }

            ',' if depth == 1 => key_start = i + 1,

            '=' if depth == 1 => {
                let key = line[key_start..i].trim();

                if key.chars().all(|c| c.is_alphanumeric() || c == '_') && !key.is_empty() {
                    field = Some(key.to_string());
                }
            }

            _ => {}
        }

        if i == target {
            return field.map(|f| (f, name));
        }
    }

    None
}

/// One mistake reaches the checker through several nested ranges, so
/// one sentence lands on a line as many times as there are ranges. The
/// innermost is the one that points at the mistake, and on a line it
/// starts last, so the greatest column of a repeated sentence wins.
pub(crate) fn keep_innermost(diagnostics: &mut Vec<TypeDiag>) {
    let mut best: HashMap<(PathBuf, usize, String), usize> = HashMap::new();

    for d in diagnostics.iter() {
        let key = (d.rel.clone(), d.line, d.message.clone());
        let col = best.entry(key).or_insert(d.col);
        *col = (*col).max(d.col);
    }

    let mut seen: HashSet<(PathBuf, usize, String)> = HashSet::new();

    diagnostics.retain(|d| {
        let key = (d.rel.clone(), d.line, d.message.clone());

        if best.get(&key) != Some(&d.col) {
            return false;
        }

        seen.insert(key)
    });
}

/// The `UnknownModule` report for a require the checker could not
/// resolve, from the module path the source wrote and the source's own
/// path relative to the root: what was asked for, and where it was
/// looked for. The kind is the caller's prefix.
pub fn unknown_module_message(
    spec: &str,
    source_rel: &Path,
    alias_target: Option<&Path>,
) -> String {
    // A data path names one file; a module path names one of several.
    let what = match crate::data::Format::of(spec) {
        Some(format) => format!("no {} file", format.name()),

        None => "no .aly, .alx, or .luau file".to_string(),
    };
    let shown = |p: &Path| p.to_string_lossy().replace('\\', "/");

    if let Some(rest) = spec.strip_prefix('@') {
        let alias = rest.split('/').next().unwrap_or(rest);

        // `@game` is reserved, so "declare the alias" is the wrong
        // advice here: the path is a service or a place in the tree.
        if alias == "game" {
            return format!(
                "\"{spec}\" names no module; `{alias_name}` names a service, `{alias_name}/Players`, \
                 or a place in the tree, `{alias_name}/ReplicatedStorage/Shared/economy`",
                alias_name = crate::game_import::ALIAS
            );
        }

        // The project declares the alias, so the folder is the answer;
        // without it the alias itself is what to add.
        return match alias_target {
            Some(target) => format!("\"{spec}\" names no module; {what} at {}", shown(target)),

            None => format!(
                "\"{spec}\" names no module; no alias {alias} in alloy.toml's [mount] table, .config.luau, or .luaurc"
            ),
        };
    }

    let base = source_rel.parent().unwrap_or(Path::new(""));
    let mut target = PathBuf::new();

    for c in base.join(spec).components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                if !target.pop() {
                    target.push("..");
                }
            }

            other => target.push(other),
        }
    }

    format!("\"{spec}\" names no module; {what} at {}", shown(&target))
}

/// The content of the first quoted string on a zero-based line.
pub fn quoted_on_line(source: &str, line: usize) -> Option<String> {
    quoted_paths_on_line(source, line).into_iter().next()
}

/// Every quoted string on a zero-based line, in the order they read.
pub fn quoted_paths_on_line(source: &str, line: usize) -> Vec<String> {
    let Some(text) = source.lines().nth(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[open] as char;
        let body = &rest[open + 1..];

        let Some(close) = body.find(quote) else {
            break;
        };

        out.push(body[..close].to_string());
        rest = &body[close + 1..];
    }

    out
}

/// The span of the runtime require the emit writes at the head of a
/// file, `local __alloy = require(...)`, on one line of the emitted
/// text, as byte columns. Alloy writes that require, and the reader
/// wrote no import of it, so a report inside the span names nothing to
/// fix.
pub fn runtime_require_span(emitted_line: &str) -> Option<(usize, usize)> {
    const PRELUDE: &str = "local __alloy = require(";
    let at = emitted_line.find(PRELUDE)?;
    let close = emitted_line[at..].find(')')?;

    Some((at, at + close))
}

/// The module path a checker's `Unknown require` is about, among the
/// quoted paths of the source line. The message names the file the
/// checker looked for, so the path whose tail that file's path ends
/// with is the one the report describes: one emitted line can carry
/// two requires. The first quoted path answers when none matches.
pub fn required_spec(message: &str, source: &str, line: usize) -> Option<String> {
    let specs = quoted_paths_on_line(source, line);

    if let Some(named) = named_module_path(message) {
        let named = named.replace('\\', "/");
        // The checker names a file, `.../src/@pkg/fluid.lua`; the source
        // wrote the path without the extension.
        let named = match named.rsplit_once('/') {
            Some((dir, file)) => match file.rsplit_once('.') {
                Some((stem, _)) => format!("{dir}/{stem}"),

                None => named.clone(),
            },

            None => named.clone(),
        };

        if let Some(hit) = specs.iter().find(|spec| {
            let tail = module_tail(spec);

            !tail.is_empty() && (named == tail || named.ends_with(&format!("/{tail}")))
        }) {
            return Some(hit.clone());
        }
    }

    specs.into_iter().next()
}

/// The file path a checker's report about a require names: the one it
/// looked for, or the one it could not take a value from.
fn named_module_path(message: &str) -> Option<&str> {
    if let Some((_, rest)) = message.rsplit_once("Unknown require: ") {
        return Some(rest.trim());
    }

    if let Some((_, rest)) = message.split_once("Cannot require module ") {
        return Some(rest.split_once(": ").map(|(p, _)| p).unwrap_or(rest).trim());
    }

    None
}

/// The text a checker writes for a module that returns no value.
pub const NO_MODULE_RETURN: &str = "Module does not return exactly 1 value";

/// The report for an import of a module that returns nothing. Luau
/// takes exactly one value from a module, so a file with no `return`
/// and no `export` gives the import nothing. A plain `.luau` file has
/// no export table, so only a `return` answers there.
pub fn no_module_return_message(spec: &str, luau: bool) -> String {
    if luau {
        return format!("\"{spec}\" returns nothing to import; add a `return`");
    }

    format!("\"{spec}\" returns nothing to import; add a `return` or an `export`")
}

/// The report for an import of a `.server` or `.client` file. Roblox
/// runs a script on its own, and a script returns nothing.
pub fn script_import_message(spec: &str) -> String {
    format!(
        "\"{spec}\" is a script, not a module; a `.server` or `.client` file runs on its own and returns nothing"
    )
}

/// A module path with its leading `./` and `../` steps and any data
/// extension dropped: what the end of the file path the checker names
/// reads as.
fn module_tail(spec: &str) -> &str {
    let mut tail = spec.trim();

    while let Some(rest) = tail.strip_prefix("./").or_else(|| tail.strip_prefix("../")) {
        tail = rest;
    }

    match crate::data::Format::of(tail) {
        Some(_) => tail.rsplit_once('.').map(|(a, _)| a).unwrap_or(tail),

        None => tail,
    }
}

/// The one-based column of the name a report quotes, when the line
/// holds it. Only the phrases that quote a name the source wrote count;
/// a quoted type is not a place.
pub(crate) fn named_column(text: &str, message: &str) -> Option<usize> {
    const OPENERS: [&str; 6] = [
        "Unknown global '",
        "Unknown type '",
        "Key '",
        "does not have key '",
        "Cannot add property '",
        "Variable '",
    ];

    let name = OPENERS
        .iter()
        .filter_map(|opener| quoted_after(message, opener))
        .find(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))?;

    word_column(text, name)
}

/// A report the checker sited on the wrong token, or worded in the
/// terms of the emit. The answer is the kind, the message, and, when
/// the report moves, its one-based line and column.
///
/// The language server calls it too, so the terminal and the editor say
/// one thing.
pub fn resite_report(
    message: &str,
    shapes: &[crate::declarations::Shape],
    source: &str,
    line: usize,
    col: usize,
) -> Option<Resited> {
    let text = source.lines().nth(line.saturating_sub(1))?;

    struct_field_report(message, shapes, source, text, line)
        .or_else(|| duplicate_declaration(message, source, line))
        .or_else(|| unknown_type_report(message, source, text, line))
        .or_else(|| variant_call_report(message, shapes, text, line, col))
        .or_else(|| array_element_report(message, text, line, col))
        .or_else(|| unmet_bound_report(message, source, text, line, col))
        .or_else(|| covered_arm_report(message, text, line))
        .or_else(|| destroy_report(message, text))
        .or_else(|| contains_report(message, text))
        .or_else(|| after_report(message, text))
}

/// `x in t` on a value the std cannot search. The emit calls `contains`,
/// whose parameter names the shapes it dispatches on; the reader wrote
/// `in`, so the sentence names the word and the operand.
fn contains_report(message: &str, text: &str) -> Option<Resited> {
    // `Container` once the fold names the union; the union itself until
    // then.
    if !matches!(
        quoted_after(message, "Expected this to be '")?,
        "Container" | "string | {}"
    ) {
        return None;
    }

    let got = quoted_after(message.split_once("but got ")?.1, "'")?;
    // The right side of the last `in` on the line. A line with two is
    // rare, and the operand is what the reader has to change.
    let rest = text.rsplit_once(" in ")?.1.trim_start();
    let operand: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    if operand.is_empty() {
        return None;
    }

    Some(Resited {
        kind: "TypeError",
        message: format!(
            "`in` needs an Array, a Set, a HashMap, or a string; `{operand}` is a {got}"
        ),
        at: None,
    })
}

/// The seconds of `after`. The emit hands them to `task.delay` or to
/// Debris, and both take an optional number, so the checker asks for a
/// `number?` where the source says a number.
fn after_report(message: &str, text: &str) -> Option<Resited> {
    if quoted_after(message, "Expected this to be '")? != "number?" {
        return None;
    }

    let got = quoted_after(message.split_once("but got ")?.1, "'")?;
    let seconds = after_seconds(text)?;

    Some(Resited {
        kind: "TypeError",
        message: format!("`after` needs a number of seconds; `{seconds}` is a {got}"),
        at: None,
    })
}

/// The text between `after` and the `do` or the `where` that ends it.
/// `destroy x after n` has neither, so the seconds run to the line end.
fn after_seconds(text: &str) -> Option<&str> {
    let body = text.trim_start();
    let rest = match body.strip_prefix("after ") {
        Some(rest) => rest,

        None => body.split_once(" after ")?.1,
    };
    let end = rest
        .find(" do")
        .or_else(|| rest.find(" where"))
        .unwrap_or(rest.len());

    Some(rest[..end].trim())
}

/// `destroy x` on a value that has no destroy method. The checker names
/// the union the std declares; the reader wrote `destroy`, so the
/// sentence names the word and the operand.
fn destroy_report(message: &str, text: &str) -> Option<Resited> {
    if quoted_after(message, "Expected this to be '")? != "Destroyable" {
        return None;
    }

    let got = quoted_after(message.split_once("but got ")?.1, "'")?;
    let operand = text.trim_start().strip_prefix("destroy ")?.trim();
    // `destroy x after n` reports on the operand, so the seconds go.
    let operand = operand.split(" after ").next()?.trim();

    Some(Resited {
        kind: "TypeError",
        message: format!(
            "`destroy` needs an Instance or a value with a destroy method; `{operand}` is a {got}"
        ),
        at: None,
    })
}

/// A `case` an arm above already covers. The emit tests the arms in
/// order, so the checker narrows the value away and reports the last
/// test as a comparison of types that cannot meet, in a negation the
/// source cannot write.
fn covered_arm_report(message: &str, text: &str, line: usize) -> Option<Resited> {
    if !(message.contains("cannot be compared with ==") && message.contains("~\"")) {
        return None;
    }

    let body = text.trim_start();
    let value = body.strip_prefix("case ")?.split_whitespace().next()?;

    Some(Resited {
        kind: "ExhaustiveMatch",
        message: format!("this arm never runs: an arm above already covers {value}"),
        at: Some((line, text.len() - body.len() + 1)),
    })
}

/// An array literal whose elements have the wrong type. The checker
/// checks the element type in both directions and against the optional
/// the array's own methods carry, so one literal draws three reports,
/// all on the call. The bracket is the mistake.
fn array_element_report(message: &str, text: &str, line: usize, col: usize) -> Option<Resited> {
    let want = quoted_after(message, "Expected this to be exactly '")?;
    let got = quoted_after(message.split_once("but got ")?.1, "'")?;
    let (want, got) = (want.trim_end_matches('?'), got.trim_end_matches('?'));
    let open = text.get(col.saturating_sub(1)..)?.find('[')?;

    Some(Resited {
        kind: "TypeError",
        message: format!("Expected this to be a `{want}[]`, but got a `{got}[]`"),
        at: Some((line, col + open)),
    })
}

/// An argument that does not meet a generic bound. The checker knows
/// the bound as the type the parameter was replaced by, so it reads as
/// a plain mismatch and sits on the call.
fn unmet_bound_report(
    message: &str,
    source: &str,
    text: &str,
    line: usize,
    col: usize,
) -> Option<Resited> {
    let want = quoted_after(message, "Expected this to be '")?;
    let got = quoted_after(message.split_once("but got ")?.1, "'")?;

    if !declares_trait(source, want) {
        return None;
    }

    let rest = text.get(col.saturating_sub(1)..)?;
    let open = rest.find('(')?;
    let lead = rest[open + 1..].len() - rest[open + 1..].trim_start().len();

    Some(Resited {
        kind: "TypeError",
        message: format!("`{got}` does not satisfy the bound `{want}`"),
        at: Some((line, col + open + 1 + lead)),
    })
}

/// Whether the source declares the name as a trait, which Alloy writes
/// as a bound and nowhere else.
fn declares_trait(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        let body = line.trim_start();
        let body = body.strip_prefix("export ").unwrap_or(body);

        body.strip_prefix("trait ").is_some_and(|rest| {
            rest.strip_prefix(name)
                .is_some_and(|tail| !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
    })
}

/// What `resite_report` answers: the kind the report carries after the
/// rewrite, its text, and the place it moves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resited {
    pub kind: &'static str,
    pub message: String,
    /// One-based line and column, when the report moves.
    pub at: Option<(usize, usize)>,
}

/// A member a struct does not have, read, written, or called. The
/// checker names the struct's runtime table and puts the report on the
/// base of the access; the reader wants the member's own name, and the
/// members that do exist.
fn struct_field_report(
    message: &str,
    shapes: &[crate::declarations::Shape],
    source: &str,
    text: &str,
    line: usize,
) -> Option<Resited> {
    let read = quoted_after(message, "Type '").zip(quoted_after(message, "does not have key '"));
    let write = quoted_after(message, "Cannot add property '")
        .zip(quoted_after(message, "' to table '"))
        .map(|(key, owner)| (owner, key));
    let (owner, key) = read.or(write)?;
    let fields = shapes.iter().find_map(|s| match s {
        crate::declarations::Shape::Struct { name, fields, .. } if name == owner => Some(fields),

        _ => None,
    })?;
    let at = member_column(text, key);
    let called = at.is_some_and(|c| text.as_bytes().get(c.saturating_sub(2)) == Some(&b':'));
    let at = at.map(|c| (line, c));
    let methods = impl_methods(source, owner);
    let private =
        fields.iter().any(|(n, p)| n == key && *p) || methods.iter().any(|(n, p)| n == key && *p);

    if private {
        return Some(Resited {
            kind: "StructError",
            message: format!("`{key}` is private to `{owner}`; only its impl reaches it"),
            at,
        });
    }

    // A member the source does write belongs to a report of its own; a
    // list of the others would not help.
    if methods.iter().any(|(n, _)| n == key) {
        return None;
    }

    let (noun, names): (&str, Vec<&str>) = match called {
        true => ("method", methods.iter().map(|(n, _)| n.as_str()).collect()),

        false => (
            "field",
            fields
                .iter()
                .filter(|(_, private)| !private)
                .map(|(n, _)| n.as_str())
                .collect(),
        ),
    };
    // A typo is the common case, and the nearest name answers it. A
    // private field is a candidate: the reader inside the impl sees it.
    // The key itself is not: two files may declare a struct of the same
    // name, and "did you mean `x`?" about `x` reads as nonsense.
    let near = fields
        .iter()
        .map(|(n, _)| n.as_str())
        .chain(methods.iter().map(|(n, _)| n.as_str()))
        .map(|n| (edit_distance(n, key), n))
        .filter(|(d, _)| *d > 0 && *d <= 2 && *d < key.len())
        .min();

    let tail = match near {
        Some((_, n)) => format!("; did you mean `{n}`?"),

        None if names.is_empty() => String::new(),

        None => format!("; its {noun}s are {}", crate::desugar::list_names(&names)),
    };

    Some(Resited {
        kind: "StructError",
        message: format!("`{owner}` has no {noun} `{key}`{tail}"),
        at,
    })
}

/// The methods an `impl` of the owner declares, each with whether it is
/// private.
fn impl_methods(source: &str, owner: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut inside = false;

    for line in source.lines() {
        let body = line.trim();

        if let Some(rest) = body.strip_prefix("impl ") {
            let target = rest.rsplit(" for ").next().unwrap_or(rest).trim();
            inside = target
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .eq(owner.chars());

            continue;
        }

        if !inside {
            continue;
        }

        if body == "end" && !line.starts_with([' ', '\t']) {
            inside = false;

            continue;
        }

        let private = body.starts_with("private ");
        let head = body.strip_prefix("private ").unwrap_or(body);

        if let Some(rest) = head.strip_prefix("function ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if !name.is_empty() && rest[name.len()..].starts_with(['(', '<']) {
                out.push((name, private));
            }
        }
    }

    out
}

/// A name declared twice. The checker reports on the `end` of the
/// second declaration and names the `end` of the first; the reader
/// looks for the two names.
fn duplicate_declaration(message: &str, source: &str, line: usize) -> Option<Resited> {
    let name = quoted_after(message, "Redefinition of type '")?;
    let decls = type_declarations(source, name);
    let second = decls
        .iter()
        .rposition(|(at, _)| *at <= line)
        .filter(|i| *i > 0)?;
    let (at, col) = decls[second];

    Some(Resited {
        kind: "TypeError",
        message: format!(
            "`{name}` is already declared, on line {}",
            decls[second - 1].0
        ),
        at: Some((at, col)),
    })
}

/// Every place a source declares a type name, as one-based line and
/// column of the name.
fn type_declarations(source: &str, name: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();

    for (i, line) in source.lines().enumerate() {
        let body = line.trim_start();
        let body = body.strip_prefix("export ").unwrap_or(body);

        for head in ["struct ", "enum ", "type ", "interface ", "trait "] {
            let Some(rest) = body.strip_prefix(head) else {
                continue;
            };

            if rest
                .strip_prefix(name)
                .is_some_and(|tail| !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
                && let Some(at) = line.find(&format!("{head}{name}"))
            {
                out.push((i + 1, at + head.len() + 1));
            }
        }
    }

    out
}

/// `Unknown type 'N'`. The checker puts it on the token that uses the
/// type, not on the name; and when `N` is a value in scope, the mirror
/// of "`N` is a type, not a value" is the sentence.
fn unknown_type_report(message: &str, source: &str, text: &str, line: usize) -> Option<Resited> {
    let name = quoted_after(message, "Unknown type '")?;
    // A struct's header and its `end` are generated text, so a field's
    // type carries no span of its own and the map falls back to the
    // declaration's first byte. The body is where the name stands.
    let (line, text, col) = match word_column(text, name) {
        Some(col) => (line, text, col),

        None => declared_type_site(source, line, name)?,
    };
    let at = Some((line, col));

    if type_declarations(source, name).is_empty() {
        if binds_value(source, name) {
            return Some(Resited {
                kind: "TypeError",
                message: format!("`{name}` is a value, not a type"),
                at,
            });
        }

        if is_generic_bound(text, col) {
            return Some(Resited {
                kind: "TypeError",
                message: format!("`{name}` names no trait or interface; a bound needs one"),
                at,
            });
        }
    }

    Some(Resited {
        kind: "TypeError",
        message: message.to_string(),
        at,
    })
}

/// Whether the source binds the name as a value: a local, a `const`, or
/// a function.
fn binds_value(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        let body = line.trim_start();
        let body = body.strip_prefix("export ").unwrap_or(body);

        ["local function ", "local ", "const ", "function "]
            .iter()
            .any(|head| {
                body.strip_prefix(head).is_some_and(|rest| {
                    rest.strip_prefix(name).is_some_and(|tail| {
                        !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                })
            })
    })
}

/// Whether the name at a one-based column sits in a generic parameter
/// list, after the `:` that opens a bound.
fn is_generic_bound(line: &str, col: usize) -> bool {
    let Some(before) = line.get(..col.saturating_sub(1)) else {
        return false;
    };
    let head = before.trim_end();
    let Some(head) = head.strip_suffix(':') else {
        return false;
    };

    match head.rfind('<') {
        Some(open) => head.rfind('(').is_none_or(|paren| paren < open),

        None => false,
    }
}

/// An enum variant built with the wrong payload. The checker counts the
/// payload as a function's parameters, and a unit variant is a string
/// it cannot call. Alloy's own wording for a pattern says it.
fn variant_call_report(
    message: &str,
    shapes: &[crate::declarations::Shape],
    text: &str,
    line: usize,
    col: usize,
) -> Option<Resited> {
    let (sep, receiver, member) = call_head(text, col)?;

    if sep != '.' {
        return None;
    }

    let payload = shapes.iter().find_map(|s| match s {
        crate::declarations::Shape::Enum { name, variants } if *name == receiver => variants
            .iter()
            .find(|(v, _)| *v == member)
            .map(|(_, types)| types.len()),

        _ => None,
    })?;
    let at = member_column(text, &member).map(|c| (line, c));

    if message.starts_with("Cannot call a value of type") {
        return (payload == 0).then(|| Resited {
            kind: "EnumError",
            message: format!("the variant `{member}` carries no payload"),
            at,
        });
    }

    let (_, given) = arity_counts(message).filter(|_| message.contains("Function expects"))?;
    let plural = if payload == 1 { "value" } else { "values" };
    let tail = match given {
        0 => "none given".to_string(),

        n => format!("{n} given"),
    };

    Some(Resited {
        kind: "EnumError",
        message: format!("the variant `{member}` carries {payload} {plural}, {tail}"),
        at,
    })
}

/// The one-based column of `.name` or `:name` on a line, at the name.
fn member_column(line: &str, name: &str) -> Option<usize> {
    [format!(".{name}"), format!(":{name}")]
        .iter()
        .filter_map(|needle| {
            line.match_indices(needle.as_str())
                .find(|(at, _)| {
                    !line[at + needle.len()..]
                        .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
                .map(|(at, _)| at + 2)
        })
        .min()
}

/// The one-based column of a name on a line, as a whole word.
/// The line inside a declaration block that names a type, with its
/// text and its one-based column. A report on the block's header or on
/// its `end` sites there instead.
fn declared_type_site<'s>(
    source: &'s str,
    line: usize,
    name: &str,
) -> Option<(usize, &'s str, usize)> {
    const HEADS: [&str; 6] = [
        "struct ",
        "enum ",
        "interface ",
        "class ",
        "trait ",
        "remote ",
    ];
    let lines: Vec<&str> = source.lines().collect();
    let at = line.checked_sub(1)?;

    if at >= lines.len() {
        return None;
    }

    let opens = |text: &str| {
        let body = text.trim_start();
        let body = body.strip_prefix("export ").unwrap_or(body);
        let body = body.strip_prefix("global ").unwrap_or(body);

        HEADS.iter().any(|h| body.starts_with(h))
    };
    let head = (0..=at).rev().find(|i| opens(lines[*i]))?;
    let close = (head + 1..lines.len()).find(|i| lines[*i].trim() == "end")?;

    if line > close + 1 {
        return None;
    }

    (head + 1..close).find_map(|i| word_column(lines[i], name).map(|col| (i + 1, lines[i], col)))
}

fn word_column(line: &str, name: &str) -> Option<usize> {
    line.match_indices(name)
        .find(|(at, _)| {
            !line[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                && !line[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .map(|(at, _)| at + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resited(message: &str, source: &str, line: usize, col: usize) -> Resited {
        let shapes = crate::declarations::shapes(source);

        resite_report(message, &shapes, source, line, col).expect("a rewrite")
    }

    const STRUCT_SRC: &str = "struct Loadout as\n    weapon: string\n    ammo: number\nend\n\nlocal kit = new Loadout { weapon = \"Bow\", ammo = 12 }\nlocal n = kit.amo\nkit.wepon = \"Sword\"\n";

    /// `destroy` and `after` each report in their own words: the
    /// checker names the std's union and the optional the timer takes,
    /// and neither is what the source wrote.
    /// `in` on a value the std cannot search read the union the
    /// parameter names. The reader wrote `in`.
    #[test]
    fn in_on_a_value_that_holds_nothing_reports_in_its_own_words() {
        let src = "local n = 5\nlocal found = 1 in n\nprint(found)\n";

        for shape in ["string | {}", "Container"] {
            let got = resited(
                &format!("Expected this to be '{shape}', but got 'number'"),
                src,
                2,
                14,
            );

            assert_eq!(got.kind, "TypeError");
            assert_eq!(
                got.message,
                "`in` needs an Array, a Set, a HashMap, or a string; `n` is a number"
            );
            assert_eq!(got.at, None);
        }

        // A dotted right side names the path the reader wrote.
        let src = "local found = 1 in state.count\nprint(found)\n";
        let got = resited(
            "Expected this to be 'Container', but got 'number'",
            src,
            1,
            15,
        );

        assert_eq!(
            got.message,
            "`in` needs an Array, a Set, a HashMap, or a string; `state.count` is a number"
        );
    }

    /// `Chat:call(...)` with a colon is the mistake a reader makes; the
    /// report named `Remote`, the emit's own name for the surface.
    #[test]
    fn a_remote_member_reports_through_either_separator() {
        let table = "{ fire: (string) -> (), instance: Instance?, on: (any) -> RBXScriptConnection, spec: any }";
        let message = format!("Key 'call' not found in table '{table}'");

        assert_eq!(
            rewrite_remote_key(&message, "    Chat.call(\"hi\")"),
            Some("remote `Chat` has no `call`".to_string())
        );
        assert_eq!(
            rewrite_remote_key(&message, "    Chat:call(\"hi\")"),
            Some("remote `Chat` has no `call`".to_string())
        );
    }

    #[test]
    fn destroy_and_after_report_in_their_own_words() {
        let src = "local count = 3\ndestroy count\n";
        let got = resited(
            "Expected this to be 'Destroyable', but got 'number'",
            src,
            2,
            9,
        );

        assert_eq!(got.kind, "TypeError");
        assert_eq!(
            got.message,
            "`destroy` needs an Instance or a value with a destroy method; `count` is a number"
        );
        assert_eq!(got.at, None);

        // An optional Instance is not one: the reader has to narrow it.
        let src = "local maybe: Part? = nil\ndestroy maybe\n";
        let got = resited(
            "Expected this to be 'Destroyable', but got 'Part?'",
            src,
            2,
            9,
        );

        assert_eq!(
            got.message,
            "`destroy` needs an Instance or a value with a destroy method; `maybe` is a Part?"
        );

        let src = "local part = Instance.new(\"Part\")\ndestroy part after \"soon\"\n";
        let got = resited(
            "Expected this to be 'number?', but got 'string'",
            src,
            2,
            20,
        );

        assert_eq!(
            got.message,
            "`after` needs a number of seconds; `\"soon\"` is a string"
        );

        let src = "after \"late\" do\n    print(1)\nend\n";
        let got = resited("Expected this to be 'number?', but got 'string'", src, 1, 7);

        assert_eq!(
            got.message,
            "`after` needs a number of seconds; `\"late\"` is a string"
        );

        // A `number?` on a line that writes no `after` is not this.
        let src = "local n: number? = f(1)\n";
        assert!(
            resite_report(
                "Expected this to be 'number?', but got 'string'",
                &[],
                src,
                1,
                20,
            )
            .is_none()
        );
    }

    #[test]
    fn a_field_read_names_the_field_and_its_place() {
        let got = resited("Type 'Loadout' does not have key 'amo'", STRUCT_SRC, 7, 11);

        assert_eq!(got.kind, "StructError");
        assert_eq!(
            got.message,
            "`Loadout` has no field `amo`; did you mean `ammo`?"
        );
        assert_eq!(got.at, Some((7, 15)));
    }

    #[test]
    fn a_field_write_reads_as_a_field_not_a_table() {
        let got = resited(
            "Cannot add property 'wepon' to table 'Loadout'",
            STRUCT_SRC,
            8,
            1,
        );

        assert_eq!(
            got.message,
            "`Loadout` has no field `wepon`; did you mean `weapon`?"
        );
        assert_eq!(got.at, Some((8, 5)));
    }

    /// Two files may declare a struct of one name, and the shape the
    /// report reads may be the other one's. The suggestion is then the
    /// key itself, which says nothing.
    #[test]
    fn a_suggestion_is_never_the_name_it_is_about() {
        let source = "struct Point as\n    x: number\n    y: number\nend\nlocal p = new Point { x = 1, y = 2 }\nprint(p.z)\n";
        let got = resited("Type 'Point' does not have key 'z'", source, 6, 7);

        assert_eq!(
            got.message,
            "`Point` has no field `z`; its fields are `x` and `y`"
        );
    }

    #[test]
    fn a_field_with_no_near_name_lists_the_fields() {
        let source = "struct Point as\n    x: number\n    y: number\nend\nlocal p = new Point { x = 1, y = 2 }\nprint(p.nothing)\n";
        let got = resited("Type 'Point' does not have key 'nothing'", source, 6, 7);

        assert_eq!(
            got.message,
            "`Point` has no field `nothing`; its fields are `x` and `y`"
        );
    }

    #[test]
    fn a_private_member_reads_as_private_and_not_as_missing() {
        let source = "struct Cooldown as\n    read name: string\n    private last: number = 0\nend\n\nimpl Cooldown as\n    private function stamp(self)\n    end\nend\n\nprint(c.last)\nc:stamp()\n";

        assert_eq!(
            resited("Type 'Cooldown' does not have key 'last'", source, 11, 7).message,
            "`last` is private to `Cooldown`; only its impl reaches it"
        );
        assert_eq!(
            resited("Type 'Cooldown' does not have key 'stamp'", source, 12, 1).message,
            "`stamp` is private to `Cooldown`; only its impl reaches it"
        );
    }

    #[test]
    fn a_method_the_struct_does_not_write_reads_as_a_method() {
        let source = "struct Sq as side: number end\n\nimpl Sq as\n    function area(self): number\n        return 1\n    end\nend\n\nlocal gone = s:perimeter()\n";
        let got = resited("Type 'Sq' does not have key 'perimeter'", source, 9, 14);

        assert_eq!(
            got.message,
            "`Sq` has no method `perimeter`; its methods are `area`"
        );
    }

    #[test]
    fn a_duplicate_declaration_names_both_places() {
        let source =
            "struct Item as\n    id: number\nend\n\nstruct Item as\n    name: string\nend\n";
        let got = resited(
            "Redefinition of type 'Item', previously defined at line 3",
            source,
            7,
            1,
        );

        assert_eq!(got.message, "`Item` is already declared, on line 1");
        assert_eq!(got.at, Some((5, 8)));
    }

    #[test]
    fn a_value_used_as_a_type_reads_as_a_value() {
        let source = "local scale = 2\nlocal bad: scale = 1\n";
        let got = resited("Unknown type 'scale'", source, 2, 12);

        assert_eq!(got.message, "`scale` is a value, not a type");
        assert_eq!(got.at, Some((2, 12)));
    }

    /// A struct's header and its `end` are generated text, so a field's
    /// type carries no span and the map falls back to the declaration's
    /// first byte. The report sites on the field instead.
    #[test]
    fn an_unknown_field_type_sites_inside_the_struct() {
        let source = "struct Inner as
    x: Undefined
end
";
        let head = resited("Unknown type 'Undefined'", source, 1, 1);
        assert_eq!(head.at, Some((2, 8)));

        let close = resited("Unknown type 'Undefined'", source, 3, 1);
        assert_eq!(close.at, Some((2, 8)));

        // A one-line struct sites on its own line already.
        let one = resited(
            "Unknown type 'Undefined'",
            "struct S as x: Undefined end
",
            1,
            16,
        );
        assert_eq!(one.at, Some((1, 16)));
    }

    #[test]
    fn a_bound_that_names_nothing_sits_on_the_bound() {
        let source =
            "local function shout<T: Loud>(item: T): string\n    return tostring(item)\nend\n";
        let got = resited("Unknown type 'Loud'", source, 1, 37);

        assert_eq!(
            got.message,
            "`Loud` names no trait or interface; a bound needs one"
        );
        assert_eq!(got.at, Some((1, 25)));
    }

    #[test]
    fn a_variant_built_with_the_wrong_payload_reads_as_a_variant() {
        let source = "enum Phase as\n    Lobby\n    Playing(number)\n    Over(string, number)\nend\n\nlocal p1 = Phase.Playing()\nlocal p2 = Phase.Over(\"red\")\nlocal p3 = Phase.Lobby(1)\n";

        assert_eq!(
            resited(
                "Argument count mismatch. Function expects 1 argument, but none are specified",
                source,
                7,
                12
            )
            .message,
            "the variant `Playing` carries 1 value, none given"
        );
        assert_eq!(
            resited(
                "Argument count mismatch. Function expects 2 arguments, but only 1 is specified",
                source,
                8,
                12
            )
            .message,
            "the variant `Over` carries 2 values, 1 given"
        );

        let unit = resited(
            "Cannot call a value of type \"Lobby\" in union: Phase",
            source,
            9,
            12,
        );

        assert_eq!(unit.kind, "EnumError");
        assert_eq!(unit.message, "the variant `Lobby` carries no payload");
        assert_eq!(unit.at, Some((9, 18)));
    }

    #[test]
    fn an_array_literal_of_the_wrong_element_reads_once_on_the_bracket() {
        let source =
            "local function names(xs: string[]): number\n    return #xs\nend\n\nnames([1, 2, 3])\n";
        let got = resited(
            "Expected this to be exactly 'string?', but got 'number'",
            source,
            5,
            1,
        );

        assert_eq!(
            got.message,
            "Expected this to be a `string[]`, but got a `number[]`"
        );
        assert_eq!(got.at, Some((5, 7)));
    }

    #[test]
    fn an_unmet_bound_reads_as_a_bound_on_the_argument() {
        let source = "trait Named as\n    function name(self): string\nend\n\nstruct Plain as\n    n: number\nend\n\nprint(announce(new Plain { n = 1 }))\n";
        let got = resited("Expected this to be 'Named', but got 'Plain'", source, 9, 7);

        assert_eq!(got.message, "`Plain` does not satisfy the bound `Named`");
        assert_eq!(got.at, Some((9, 16)));
    }

    #[test]
    fn an_arm_an_earlier_one_covers_reads_as_an_arm() {
        let source = "local function grade(name: string): number\n    return match name with\n        case \"gold\" then 3\n        case \"silver\" then 2\n        case \"gold\" then 1\n        default 0\n    end\nend\n";
        let got = resited(
            "Types string & ~\"gold\" & ~\"silver\" and \"gold\" cannot be compared with == because they do not have the same metatable",
            source,
            5,
            9,
        );

        assert_eq!(got.kind, "ExhaustiveMatch");
        assert_eq!(
            got.message,
            "this arm never runs: an arm above already covers \"gold\""
        );
        assert_eq!(got.at, Some((5, 9)));
    }

    #[test]
    fn the_strip_leaves_the_require_binding_whole() {
        assert_eq!(
            strip_std_prefix("Too many type parameters passed to '__alloy'"),
            "Too many type parameters passed to '__alloy'"
        );
        assert_eq!(strip_std_prefix("__alloy.Future<number>"), "Future<number>");
        assert_eq!(
            strip_std_prefix("Key 'x' not found in 'lib__all'"),
            "Key 'x' not found in 'lib'"
        );
        assert!(crate::shapes::names_only_the_emit(
            "Too many type parameters passed to '__alloy', which is typed as <T>(...any) -> T[]"
        ));
    }

    #[test]
    fn a_report_off_its_line_keeps_the_name_the_reader_wrote() {
        let text = "            <TextLabel Size={12} Text={props_missing} />";

        assert_eq!(
            named_column(
                text,
                "Unknown global 'props_missing'; consider assigning to it first"
            ),
            Some(40)
        );
        assert_eq!(named_column(text, "Expected this to be 'number'"), None);
    }

    #[test]
    fn the_report_names_the_require_the_checker_looked_for() {
        // One emitted line carries the runtime require and the import;
        // the file the checker names says which one failed.
        let source = "import fluid from '@pkg/fluid'\n";
        assert_eq!(
            required_spec("Unknown require: /m/src/@pkg/fluid.lua", source, 0),
            Some("@pkg/fluid".to_string())
        );
        // A line with two paths: the second one is the one that failed.
        let two = "import { a } from \"./ok\" import { b } from \"./gone\"\n";
        assert_eq!(
            required_spec("Unknown require: /m/src/gone.lua", two, 0),
            Some("./gone".to_string())
        );
        // A data path keeps its extension in the source, and the
        // checker names the module the build writes beside it.
        assert_eq!(
            required_spec(
                "Unknown require: /m/src/app/data.lua",
                "import d from \"./data.json\"\n",
                0
            ),
            Some("./data.json".to_string())
        );
        // Nothing to match: the first path on the line answers.
        assert_eq!(
            required_spec("Unknown require: unsupported path", two, 0),
            Some("./ok".to_string())
        );

        // The runtime require the emit writes has its own span.
        let emitted = "local __alloy = require(\"@alloy\") local fluid = require('@pkg/fluid')";
        let (start, end) = runtime_require_span(emitted).expect("a span");
        assert_eq!(&emitted[start..=end], "local __alloy = require(\"@alloy\")");
        assert!(runtime_require_span("local fluid = require('@pkg/fluid')").is_none());
    }

    #[test]
    fn an_unknown_module_names_what_was_asked_for() {
        assert_eq!(
            unknown_module_message("./ui", Path::new("src/app/main.aly"), None),
            "\"./ui\" names no module; no .aly, .alx, or .luau file at src/app/ui"
        );
        assert_eq!(
            unknown_module_message("../shared/util", Path::new("src/app/main.aly"), None),
            "\"../shared/util\" names no module; no .aly, .alx, or .luau file at src/shared/util"
        );
        // No such alias: the alias is what to add.
        assert_eq!(
            unknown_module_message("@packages/react", Path::new("src/main.aly"), None),
            "\"@packages/react\" names no module; no alias packages in alloy.toml's [mount] table, .config.luau, or .luaurc"
        );
        // The alias is declared, so the folder it names is the answer.
        assert_eq!(
            unknown_module_message(
                "@pkg/nope",
                Path::new("src/main.aly"),
                Some(Path::new("packages/roblox/nope"))
            ),
            "\"@pkg/nope\" names no module; no .aly, .alx, or .luau file at packages/roblox/nope"
        );
        assert_eq!(
            unknown_module_message("./data.json", Path::new("src/app/main.aly"), None),
            "\"./data.json\" names no module; no JSON file at src/app/data.json"
        );
        assert_eq!(
            quoted_on_line("import { a } from \"./x\"\nlocal y = 1\n", 0),
            Some("./x".to_string())
        );
    }

    #[test]
    fn await_on_a_plain_value_names_that_value() {
        let known = crate::shapes::Known::default();
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'Awaitable<T>', but got 'number'",
                &known,
                Some("local nope = await n"),
                14
            ),
            "`await` needs a Future; `number` is not one"
        );
        // A narrowed primitive prints as `typeof(string)`.
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'Awaitable<T>', but got 'typeof(string)'",
                &known,
                Some("local nope = await s"),
                14
            ),
            "`await` needs a Future; `string` is not one"
        );
        // The parameter is a bound now, and the bound prints its name.
        assert_eq!(
            friendly_type_message(
                "`number` does not satisfy the bound `Settled<any>`",
                &known,
                Some("local nope = await n"),
                14
            ),
            "`await` needs a Future; `number` is not one"
        );
    }

    /// `__value` is the key the Future type carries; the source never
    /// writes it, so the report beside the bound one goes.
    #[test]
    fn the_value_key_of_a_future_is_no_report() {
        assert!(crate::shapes::names_the_emit_key(
            "Property '\"__value\"' does not exist on type 'number'",
            "local nope = await n"
        ));
        assert!(!crate::shapes::names_the_emit_key(
            "Property '\"__value\"' does not exist on type 'number'",
            "local v = f.__value"
        ));
    }

    #[test]
    fn a_mapped_result_reads_as_a_result() {
        let known = crate::shapes::Known::default();
        let message = "Expected this to be 'number', but got 'ResultMethods2<number, string> & { read _1: number | string, read __err: string, read __ok: number, tag: \"Err\" | \"Ok\", read trace: string? }'";
        assert_eq!(
            friendly_type_message(message, &known, None, 0),
            "Expected this to be 'number', but got 'Result<number, string>'"
        );
    }

    #[test]
    fn a_method_call_message_leaves_out_self() {
        let known = crate::shapes::Known::default();
        let line = "local b = xs:len(1, 2)";
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 argument, but 3 are specified",
                &known,
                Some(line),
                11
            ),
            "Argument count mismatch. `len` takes 0 arguments, but 2 are specified"
        );
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 3 arguments, but only 2 are specified",
                &known,
                Some("local c = xs:reduce(f)"),
                11
            ),
            "Argument count mismatch. `reduce` takes 2 arguments, but only 1 is specified"
        );
    }

    #[test]
    fn a_dot_call_of_a_method_names_the_colon() {
        let known = crate::shapes::Known::default();
        assert_eq!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 argument, but none are specified",
                &known,
                Some("local a = c.bump()"),
                11
            ),
            "`bump` is a method; call it with `c:bump(...)`, not `c.bump(...)`"
        );
        // A range of counts says nothing about the separator.
        assert!(
            friendly_type_message(
                "Argument count mismatch. Function expects 1 to 2 arguments, but none are specified",
                &known,
                Some("Toast.fire_all()"),
                1
            )
            .contains("Function expects 1 to 2")
        );
    }

    #[test]
    fn a_remote_surface_reads_as_the_remote() {
        let known = crate::shapes::Known::default();
        let message = "Key 'blast' not found in table '{ call: (Player, string) -> Future<any>, fire: (Player, string) -> (), instance: Instance?, spec: any }'";
        assert_eq!(
            friendly_type_message(message, &known, Some("Toast.blast(\"x\")"), 1),
            "remote `Toast` has no `blast`"
        );
        assert_eq!(
            friendly_type_message(
                "Key 'fira' not found in table 'Remote'",
                &known,
                Some("Toast.fira(\"x\")"),
                1
            ),
            "remote `Toast` has no `fira`"
        );
    }

    #[test]
    fn a_dot_call_that_lands_on_the_self_parameter_names_the_colon() {
        assert_eq!(
            friendly_type_message(
                "TypeError: Expected this to be 'read number[]', but got 'number'",
                &crate::shapes::Known::default(),
                Some("local d = xs.push(4)"),
                19,
            ),
            "`push` is a method; call it with `xs:push(...)`, not `xs.push(...)`"
        );
    }

    /// The checker answers a `{ ... }` where an Array belongs with the
    /// nineteen methods the table lacks. The reader wrote the wrong
    /// bracket.
    #[test]
    fn a_table_literal_where_an_array_belongs_names_the_bracket() {
        let message = "Table type '{string}' not compatible with type 'string[]' because the former is missing fields 'find', 'filter', 'push', 'map'";
        assert_eq!(
            friendly_type_message(message, &crate::shapes::Known::default(), None, 1),
            "a `{ ... }` is a plain table, not a `string[]`; an Array literal is `[ ... ]`"
        );
        // A table where a table belongs keeps the checker's words.
        let other = "Table type '{string}' not compatible with type '{ x: number }' because the former is missing fields 'x'";
        assert!(
            friendly_type_message(other, &crate::shapes::Known::default(), None, 1)
                .contains("missing fields"),
        );
    }

    /// A nil base makes every key on it unknown. `could be nil` names
    /// the problem; the key report sends the reader after a typo that
    /// is not there.
    #[test]
    fn the_unused_lints_and_the_nil_cascade_belong_to_alloy() {
        assert!(owned_lint("LocalUnused"));
        assert!(owned_lint("FunctionUnused"));
        assert!(owned_lint("ImportUnused"));
        assert!(!owned_lint("DeprecatedApi"));
    }

    /// The checker names the two emitted files of an import cycle;
    /// `circular_import` names the two the author wrote.
    #[test]
    fn the_emit_only_reports_are_dropped() {
        assert!(crate::shapes::names_only_the_emit(
            "TypeError: Key '%error-id%' not found in external type 'Player'"
        ));
        assert!(crate::shapes::names_only_the_emit(
            "Cyclic module dependency: /tmp/alloy-flux-1/root/build/a.luau -> /tmp/x/b.luau"
        ));
        assert!(!crate::shapes::names_only_the_emit(
            "Key 'Position' not found in external type 'Instance'"
        ));
    }

    #[test]
    fn a_checker_lint_pairs_with_the_alloy_one() {
        assert_eq!(paired_lint("TableLiteral"), Some(&["duplicate_key"][..]));
        assert_eq!(paired_lint("LocalUnused"), Some(&["unused_variable"][..]));
        assert_eq!(
            paired_lint("UnreachableCode"),
            Some(&["unreachable_code"][..])
        );
        assert_eq!(paired_lint("TypeError"), None);
    }

    #[test]
    fn a_new_on_a_type_alias_names_the_type() {
        let src = "type Plain = { a: number }\nlocal q = new Plain { a = 1 }\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Plain'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Plain` is a type, not a struct".to_string(), None))
        );
    }

    #[test]
    fn an_is_test_against_no_type_says_so() {
        let src = "local v: any = 1\nif v is Nothing then print(\"?\") end\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Nothing'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Nothing` is not a type in scope".to_string(), None))
        );
    }

    #[test]
    fn an_impl_for_an_alias_reports_on_the_impl_line() {
        let src = "type Alias = { z: number }\nimpl Shape for Alias as\n    function area(self): number\n        return self.z\n    end\nend\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Alias'; consider assigning to it first",
                src,
                3
            ),
            Some((
                "`Alias` is a type, not a struct; `impl` needs one".to_string(),
                Some(2)
            ))
        );
    }

    #[test]
    fn a_type_printed_as_a_value_says_it_is_a_type() {
        let src = "type Alias3 = number\nprint(Alias3)\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Alias3'; consider assigning to it first",
                src,
                2
            ),
            Some(("`Alias3` is a type, not a value".to_string(), None))
        );

        let iface = "interface Both extends HasName as\n    id: number\nend\nprint(Both)\n";
        assert_eq!(
            rewrite_emitted_name(
                "Unknown global 'Both'; consider assigning to it first",
                iface,
                4
            ),
            Some(("`Both` is a type, not a value".to_string(), None))
        );
    }

    #[test]
    fn a_new_on_a_value_names_what_it_holds() {
        let src = "local n = 5\nlocal r = new n {}\n";
        assert_eq!(
            rewrite_emitted_name("Type 'number' does not have key 'new'", src, 2),
            Some(("`new` needs a struct; `n` is a number".to_string(), None))
        );
    }

    #[test]
    fn a_constructor_message_names_the_field() {
        let known = crate::shapes::Known::default();
        let line = "local bad4 = new Plain { a = \"not a number\", b = \"x\" }";
        assert_eq!(
            friendly_type_message(
                "Expected this to be 'number', but got 'string'",
                &known,
                Some(line),
                30
            ),
            "field `a` of `Plain`: Expected this to be 'number', but got 'string'"
        );
        assert_eq!(
            constructor_field(line, 30),
            Some(("a".into(), "Plain".into()))
        );
        assert_eq!(
            constructor_field(line, 50),
            Some(("b".into(), "Plain".into()))
        );
        assert_eq!(constructor_field("local p = { a = 1 }", 13), None);
    }

    #[test]
    fn a_repeated_sentence_keeps_the_innermost_range() {
        let mut diagnostics = vec![
            TypeDiag {
                rel: PathBuf::from("a.aly"),
                line: 11,
                col: 12,
                kind: "TypeError".into(),
                message: "Operator '+' could not be applied".into(),
            },
            TypeDiag {
                rel: PathBuf::from("a.aly"),
                line: 11,
                col: 55,
                kind: "TypeError".into(),
                message: "Operator '+' could not be applied".into(),
            },
        ];
        keep_innermost(&mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].col, 55);
    }
}
