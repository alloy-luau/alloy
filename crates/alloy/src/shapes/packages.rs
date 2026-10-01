/*!
The record aliases a Luau package exports. jecs declares
`export type Entity<T = nil> = { __T: T }`, and the checker prints a value
of it as the record, `{ __T: {Stack} }`. The fold matches the print against
each alias a file reaches and writes the name the file reads the alias by:
`jecs.Entity<{ Stack }>`.

A tie goes to the alias that the value's origin names, and else to the
alias that the package declares first. jecs declares `Entity`, `Id`, and
`Component` over one record, and `jecs.component` returns `Entity<T>`.
The hover moves the alias of the origin to the front of the list, so the
fold reads the list in order and takes the first match.
*/

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use super::Known;
use super::strings::{group_len, member_parts};

/// A record alias of a Luau module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAlias {
    /// The name a file reads the alias by: `jecs.Entity`, or `Entity`
    /// when the file imports the name. The module's own name in the
    /// module's list.
    pub name: String,
    /// Each type parameter, with its default.
    pub params: Vec<(String, Option<String>)>,
    /// The record, as the package writes it.
    pub body: String,
    /// The functions of the package whose return type names the alias.
    pub made_by: Vec<String>,
}

/// How many `export type X = m.X` steps the read follows. A package
/// entry file passes its types on from the real module, and that one
/// may pass them on from a module of its own.
const PASS_DEPTH: u8 = 4;

/// The files a module's list read, each with its time of change, and
/// the list.
type Entry = (Vec<(PathBuf, Option<SystemTime>)>, Arc<Vec<PackageAlias>>);

/// Every document compile asks for the aliases again, and a package
/// changes only on an install. The list keeps until a file it read
/// changes.
fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();

    CACHE.get_or_init(Default::default)
}

fn stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The record aliases a Luau module exports, in the order it declares
/// them, the ones it passes on from another module included.
pub(crate) fn module_aliases(path: &Path) -> Arc<Vec<PackageAlias>> {
    aliases_of(path, PASS_DEPTH).1
}

fn aliases_of(path: &Path, depth: u8) -> Entry {
    let held = cache().lock().ok().and_then(|c| c.get(path).cloned());

    if let Some(hit) = held
        && hit.0.iter().all(|(p, t)| stamp(p) == *t)
    {
        return hit;
    }

    let mut deps = vec![(path.to_path_buf(), stamp(path))];
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let module = parse(&text);
    let mut out: Vec<PackageAlias> = Vec::new();

    for item in module.items {
        match item {
            Item::Record(alias) => out.push(alias),

            Item::Pass {
                name,
                local,
                target,
            } if depth > 0 => {
                let Some(file) = module
                    .requires
                    .iter()
                    .find(|(l, _)| *l == local)
                    .and_then(|(_, spec)| crate::modules::resolve(spec, path, &[]))
                else {
                    continue;
                };
                let (read, list) = aliases_of(&file, depth - 1);
                deps.extend(read);

                if let Some(found) = list.iter().find(|a| a.name == target) {
                    out.push(PackageAlias {
                        name,
                        ..found.clone()
                    });
                }
            }

            Item::Pass { .. } => {}
        }
    }

    for alias in &mut out {
        for (func, returned) in &module.returns {
            if *returned == alias.name && !alias.made_by.contains(func) {
                alias.made_by.push(func.clone());
            }
        }
    }

    let entry = (deps, Arc::new(out));

    if let Ok(mut c) = cache().lock() {
        c.insert(path.to_path_buf(), entry.clone());
    }

    entry
}

enum Item {
    Record(PackageAlias),
    /// `export type Entity<T = nil> = module.Entity<T>`.
    Pass {
        name: String,
        local: String,
        target: String,
    },
}

#[derive(Default)]
struct Module {
    items: Vec<Item>,
    /// `local module = require("./x")`: the local and the path.
    requires: Vec<(String, String)>,
    /// A function and the alias its return type names.
    returns: Vec<(String, String)>,
}

fn ident_len(text: &str) -> usize {
    text.find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len())
}

/// The last name of a dotted path at the start of the text:
/// `module.Entity<T>` reads `Entity`.
fn path_tail(text: &str) -> &str {
    let len = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(text.len());

    text[..len].rsplit('.').next().unwrap_or("")
}

/// The source with its comments blanked to spaces, so every offset
/// holds and no `{` of a comment opens a record.
fn uncommented(text: &str) -> String {
    let Ok(lexed) = alloy_syntax::lexer::lex_luau(text) else {
        return text.to_string();
    };
    let mut bytes = text.as_bytes().to_vec();

    for &(start, end) in &lexed.comments {
        for b in &mut bytes[start as usize..end as usize] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }

    // Blanking whole comments keeps each multi-byte character whole or
    // replaces it whole.
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_string())
}

fn parse(source: &str) -> Module {
    let text = uncommented(source);
    let mut module = Module::default();
    let mut at = 0;

    for line in text.split_inclusive('\n') {
        let start = at + (line.len() - line.trim_start().len());
        at += line.len();
        let head = line.trim_start();

        if head.starts_with("export type ") {
            if let Some(item) = parse_alias(&text[start + "export type ".len()..]) {
                module.items.push(item);
            }
        } else if let Some(rest) = head.strip_prefix("local ")
            && let Some(req) = parse_require(rest)
        {
            module.requires.push(req);
        }

        if let Some(ret) = parse_return(head) {
            module.returns.push(ret);
        }
    }

    module
}

/// `Name<P = D> = { ... }` or `Name<P> = m.Name<P>`, from the text after
/// `export type `.
fn parse_alias(text: &str) -> Option<Item> {
    let name = &text[..ident_len(text)];

    if name.is_empty() {
        return None;
    }

    let mut rest = text[name.len()..].trim_start();
    let mut params = Vec::new();

    if rest.starts_with('<') {
        let len = group_len(rest, '<', '>')?;

        for part in super::top_level_parts(&rest[1..len - 1]) {
            let (param, default) = match part.split_once('=') {
                Some((p, d)) => (p.trim(), Some(d.trim().to_string())),

                None => (part.trim(), None),
            };

            // A pack parameter binds a list, which no record member holds.
            if param.ends_with("...") || param.is_empty() {
                return None;
            }

            params.push((param.to_string(), default));
        }

        rest = rest[len..].trim_start();
    }

    let body = rest.strip_prefix('=')?.trim_start();

    if body.starts_with('{') {
        let len = group_len(body, '{', '}')?;

        return Some(Item::Record(PackageAlias {
            name: name.to_string(),
            params,
            // Luau separates members with `;` as well as `,`.
            body: body[..len].replace(';', ","),
            made_by: Vec::new(),
        }));
    }

    // Only a pass that hands its own parameters on, in order, is the
    // same type under the same parameters.
    let (local, tail) = body.split_once('.')?;
    let target = &tail[..ident_len(tail)];
    let after = &tail[target.len()..];
    let args: Vec<&str> = match after.starts_with('<') {
        true => super::top_level_parts(&after[1..group_len(after, '<', '>')? - 1])
            .into_iter()
            .map(str::trim)
            .collect(),

        false => Vec::new(),
    };
    let straight = args.len() == params.len() && args.iter().zip(&params).all(|(a, (p, _))| a == p);

    (straight && !target.is_empty() && ident_len(local) == local.len()).then(|| Item::Pass {
        name: name.to_string(),
        local: local.to_string(),
        target: target.to_string(),
    })
}

/// `module = require("./x")`, from the text after `local `.
fn parse_require(text: &str) -> Option<(String, String)> {
    let local = &text[..ident_len(text)];
    let call = text[local.len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .strip_prefix("require(")?;
    let quote = call.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let spec = &call[1..];
    let end = spec.find(quote)?;

    (!local.is_empty()).then(|| (local.to_string(), spec[..end].to_string()))
}

/// The function a line declares and the name its return type starts
/// with: `function jecs.entity(world): Entity` and the table field
/// `component = (ECS_COMPONENT :: any) :: <T>() -> Entity<T>` both read.
/// A signature over several lines reads nothing.
fn parse_return(line: &str) -> Option<(String, String)> {
    if let Some((_, rest)) = line.split_once("function ") {
        let path_len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == ':'))
            .unwrap_or(rest.len());
        let func = rest[..path_len].rsplit(['.', ':']).next()?;
        let mut after = &rest[path_len..];

        if after.starts_with('<') {
            after = &after[group_len(after, '<', '>')?..];
        }

        let params = group_len(after, '(', ')')?;
        let returned = path_tail(after[params..].trim_start().strip_prefix(':')?.trim_start());

        return (!func.is_empty() && !returned.is_empty())
            .then(|| (func.to_string(), returned.to_string()));
    }

    let func = &line[..ident_len(line)];
    line[func.len()..].trim_start().strip_prefix('=')?;
    let (_, tail) = line.rsplit_once("->")?;
    let returned = path_tail(tail.trim_start());

    (!func.is_empty() && !returned.is_empty()).then(|| (func.to_string(), returned.to_string()))
}

impl PackageAlias {
    /// The name with the arguments that make the record read as the
    /// print, when the print is the record.
    fn name_of(&self, printed: &str) -> Option<String> {
        let parts = |text: &str| -> Vec<String> {
            member_parts(text)
                .into_iter()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        };
        let want = parts(&self.body);
        let got = parts(printed);

        // An empty record would name every `{}` of every print.
        if want.is_empty() || want.len() != got.len() {
            return None;
        }

        let names: Vec<&str> = self.params.iter().map(|(p, _)| p.as_str()).collect();
        let mut binds: Vec<Option<String>> = vec![None; names.len()];
        let mut used = vec![false; got.len()];

        for member in &want {
            let pieces = pieces(member, &names);
            let hit = (0..got.len()).find(|&i| {
                if used[i] {
                    return false;
                }

                let saved = binds.clone();
                let ok = unify(&pieces, &got[i], &mut binds);

                if !ok {
                    binds = saved;
                }

                ok
            })?;
            used[hit] = true;
        }

        // A parameter no member binds takes its default, and one with no
        // default leaves no name to write.
        let mut args: Vec<String> = self
            .params
            .iter()
            .zip(binds)
            .map(|((_, default), bound)| bound.map(|b| tidy(&b)).or_else(|| default.clone()))
            .collect::<Option<_>>()?;

        // `Entity<nil>` of `Entity<T = nil>` is `Entity`, as a reader
        // writes it.
        while let Some(last) = args.last()
            && self.params[args.len() - 1]
                .1
                .as_deref()
                .is_some_and(|d| squash(d) == squash(last))
        {
            args.pop();
        }

        Some(match args.is_empty() {
            true => self.name.clone(),

            false => format!("{}<{}>", self.name, args.join(", ")),
        })
    }
}

/// One piece of a member as the alias writes it: text to match, or a
/// type parameter that takes the type the print has there.
enum Piece<'a> {
    Text(&'a str),
    Param(usize),
}

/// A member split at each type parameter. A name before a `:` is a
/// key, not a parameter, and a name after a `.` belongs to a path.
fn pieces<'a>(member: &'a str, params: &[&str]) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut i = 0;
    let bytes = member.as_bytes();

    while i < member.len() {
        let c = bytes[i];

        if !(c.is_ascii_alphabetic() || c == b'_') {
            i += 1;

            continue;
        }

        let len = ident_len(&member[i..]);
        let word = &member[i..i + len];
        let after = member[i + len..].trim_start();
        let dotted = i > 0 && bytes[i - 1] == b'.';

        match params.iter().position(|p| *p == word) {
            Some(k) if !dotted && !after.starts_with(':') => {
                if from < i {
                    out.push(Piece::Text(&member[from..i]));
                }

                out.push(Piece::Param(k));
                from = i + len;
            }

            _ => {}
        }

        i += len;
    }

    if from < member.len() {
        out.push(Piece::Text(&member[from..]));
    }

    out
}

/// The text after `lit` at the start of `text`, where whitespace counts
/// for nothing on either side: the print writes `{Stack}` and the
/// package `{ Stack }`.
fn eat<'t>(text: &'t str, lit: &str) -> Option<&'t str> {
    let mut rest = text;

    for c in lit.chars().filter(|c| !c.is_whitespace()) {
        rest = rest.trim_start().strip_prefix(c)?;
    }

    Some(rest)
}

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether the pieces read as the text, with each parameter bound to
/// one type. A parameter the member names twice binds one type.
fn unify(pieces: &[Piece], text: &str, binds: &mut [Option<String>]) -> bool {
    let Some((first, rest)) = pieces.split_first() else {
        return text.trim().is_empty();
    };

    match first {
        Piece::Text(lit) => eat(text, lit).is_some_and(|t| unify(rest, t, binds)),

        Piece::Param(k) => {
            if let Some(bound) = binds[*k].clone() {
                return eat(text, &bound).is_some_and(|t| unify(rest, t, binds));
            }

            for end in type_ends(text) {
                binds[*k] = Some(text[..end].trim().to_string());

                if unify(rest, &text[end..], binds) {
                    return true;
                }
            }

            binds[*k] = None;

            false
        }
    }
}

/// Each place a type that starts the text may end: outside every
/// bracket and quote, and not inside a name. A `,` or a `:` outside
/// the brackets ends every type.
fn type_ends(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    let word = |c: char| c.is_alphanumeric() || c == '_';

    for (k, c) in text.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }

            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '{' | '[' => depth += 1,
                '<' if word(prev) => depth += 1,
                ')' | '}' | ']' if depth == 0 => break,
                ')' | '}' | ']' => depth -= 1,
                '>' if prev != '-' && depth == 0 => break,
                '>' if prev != '-' => depth -= 1,
                ',' | ':' if depth == 0 => break,
                _ => {}
            },
        }

        prev = c;
        let end = k + c.len_utf8();
        let splits_a_name = word(c) && text[end..].starts_with(word);

        if depth == 0 && quote.is_none() && !splits_a_name && !text[..end].trim().is_empty() {
            out.push(end);
        }
    }

    out
}

/// A type argument on one line, with the spaces inside its braces the
/// source writes: `{Stack}` reads `{ Stack }`.
fn tidy(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(flat.len() + 4);
    let chars: Vec<char> = flat.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if c == '}' && i > 0 && !matches!(chars[i - 1], ' ' | '{') {
            out.push(' ');
        }

        out.push(c);

        if c == '{' && !matches!(chars.get(i + 1), Some(' ' | '}') | None) {
            out.push(' ');
        }
    }

    out
}

/// Every record of the text that a package alias holds reads by the
/// alias's name. The walk goes from the last `{` back, so an inner
/// record folds before the one around it.
pub(crate) fn fold_package_aliases(text: &mut String, known: &Known) {
    if known.aliases.is_empty() {
        return;
    }

    let mut end = text.len();

    while let Some(open) = text[..end].rfind('{') {
        end = open;

        let Some(len) = group_len(&text[open..], '{', '}') else {
            continue;
        };
        let body = &text[open..open + len];

        if let Some(name) = known.aliases.iter().find_map(|a| a.name_of(body)) {
            text.replace_range(open..open + len, &name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(name: &str, params: &[(&str, Option<&str>)], body: &str) -> PackageAlias {
        PackageAlias {
            name: name.to_string(),
            params: params
                .iter()
                .map(|(p, d)| (p.to_string(), d.map(str::to_string)))
                .collect(),
            body: body.to_string(),
            made_by: Vec::new(),
        }
    }

    #[test]
    fn a_record_reads_as_the_alias_with_its_arguments() {
        let entity = alias("jecs.Entity", &[("T", Some("nil"))], "{ __T: T }");
        let map = alias(
            "pkg.Map",
            &[("K", None), ("V", None)],
            "{ [K]: V, size: (self: Map<K, V>) -> number }",
        );

        assert_eq!(
            entity.name_of("{\n    __T: {Stack}\n}").as_deref(),
            Some("jecs.Entity<{ Stack }>")
        );
        assert_eq!(
            entity.name_of("{ __T: nil }").as_deref(),
            Some("jecs.Entity")
        );
        assert_eq!(entity.name_of("{ __T: number, x: number }"), None);
        assert_eq!(
            map.name_of("{ size: (self: Map<string, Part>) -> number, [string]: Part }")
                .as_deref(),
            Some("pkg.Map<string, Part>")
        );
        // One parameter binds one type.
        assert_eq!(
            map.name_of("{ [string]: Part, size: (self: Map<number, Part>) -> number }"),
            None
        );
    }

    #[test]
    fn a_module_passes_its_aliases_on_and_names_their_makers() {
        let module = parse(concat!(
            "local module = require(\"./src/lib\")\n",
            "-- export type Hidden = { x: number }\n",
            "export type Entity<T = nil> = module.Entity<T>\n",
            "export type Fixed = module.Entity<number>\n",
            "export type Row = {\n    id: number;\n    name: string,\n}\n",
            "export type Pack<T...> = { f: (T...) -> () }\n",
            "return {\n    component = (C :: any) :: <T>() -> Entity<T>,\n}\n",
        ));

        assert_eq!(
            module.requires,
            vec![("module".to_string(), "./src/lib".to_string())]
        );
        assert_eq!(
            module.returns,
            vec![("component".to_string(), "Entity".to_string())]
        );

        let names: Vec<String> = module
            .items
            .iter()
            .map(|i| match i {
                Item::Record(a) => a.name.clone(),

                Item::Pass { name, .. } => format!("pass {name}"),
            })
            .collect();

        assert_eq!(names, vec!["pass Entity", "Row"]);
    }
}
