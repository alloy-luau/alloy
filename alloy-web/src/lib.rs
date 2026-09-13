//! The playground's compiler: the desugar, the completion contexts,
//! the keyword and declaration hovers, and the shape folds, as wasm.
//! The page pairs it with Luau's analyzer, also wasm, which reads the
//! check artifact; the span map here turns its positions back into
//! the source's. One session at a time: `set_source` compiles, and the
//! other calls read what it compiled.

#[allow(dead_code)]
#[path = "../../alloy-lsp/src/context/mod.rs"]
mod context;
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/keywords.rs"]
mod keywords;
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/names.rs"]
mod names;
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/shapes/mod.rs"]
mod shapes;

use std::cell::RefCell;

use alloy::declarations::{Declaration, Shape};
use alloy::{EmitOptions, Output};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use context::Context;
use names::{EXPRESSION_GLOBALS, builtin_attribute_targets, declares_a_name_at};

struct Session {
    source: String,
    output: Option<Output>,
    decls: Vec<Declaration>,
    shapes: Vec<Shape>,
    interfaces: Vec<shapes::Interface>,
}

thread_local! {
    static SESSION: RefCell<Session> = const {
        RefCell::new(Session {
            source: String::new(),
            output: None,
            decls: Vec::new(),
            shapes: Vec::new(),
            interfaces: Vec::new(),
        })
    };
}

/// The runtime the check artifact requires as `@alloy`.
#[wasm_bindgen]
pub fn runtime() -> String {
    alloy::RUNTIME.to_string()
}

/// Compiles the source and keeps it. The JSON holds the ship and check
/// artifacts, the compiler's diagnostics, and the lints, each with byte
/// offsets in the source; a source that does not parse gives `error`.
#[wasm_bindgen]
pub fn set_source(source: &str) -> String {
    let options = EmitOptions {
        file_name: "play.aly".to_string(),
        ..EmitOptions::default()
    };
    let compiled = alloy::compile_with(source, &options);
    let decls = alloy::declarations::summaries(source, false);
    let shapes = alloy::declarations::shapes(source);
    // The playground is a scratch pad, not a game: the pedantic group
    // stays quiet there, so a `print` draws no warning.
    let lint_config = alloy::config::LintConfig::default().without_strict();
    // `--@alloy-lint` in the file wins over the defaults, and
    // `--@alloy-preserve` keeps a rewrite off a line.
    let directives = alloy::directives::scan(source);

    let result = match &compiled {
        Ok(out) => {
            let diagnostics: Vec<Value> = out
                .diagnostics
                .iter()
                .map(|d| {
                    json!({
                        "start": d.start,
                        "end": d.end.max(d.start),
                        "message": alloy::docs::labeled(&d.message),
                        "code": alloy::docs::code_for(&d.message),
                    })
                })
                .collect();
            let lints: Vec<Value> = out
                .lints
                .iter()
                .filter_map(|l| {
                    let level = match alloy::lint::level_in(&lint_config, &directives, l.name) {
                        alloy::lint::Level::Allow => return None,
                        alloy::lint::Level::Warn => "warning",
                        alloy::lint::Level::Deny => "error",
                    };
                    let fix = l.fix.as_ref().filter(|f| {
                        !directives.preserves(alloy::directives::line_of(source, f.start as usize))
                    });

                    Some(json!({
                        "name": l.name,
                        "level": level,
                        "start": l.start,
                        "end": l.end.max(l.start),
                        "message": l.message,
                        "fix": fix.map(|f| json!({ "start": f.start, "end": f.end, "replacement": f.replacement })),
                    }))
                })
                .collect();

            json!({
                "ship": out.ship,
                "check": out.check,
                "diagnostics": diagnostics,
                "lints": lints,
            })
        }

        Err(e) => json!({
            "error": { "offset": e.offset, "message": alloy::docs::labeled(&e.message) },
        }),
    };

    SESSION.with(|s| {
        let mut s = s.borrow_mut();
        s.source = source.to_string();
        s.output = compiled.ok();
        s.decls = decls;
        s.shapes = shapes;
        s.interfaces = shapes::interfaces(source);
    });

    result.to_string()
}

/// A byte offset in the check artifact as one in the source.
#[wasm_bindgen]
pub fn to_source(check_offset: u32) -> u32 {
    SESSION.with(|s| {
        s.borrow()
            .output
            .as_ref()
            .map_or(check_offset, |o| o.map.to_source(check_offset))
    })
}

/// The check-artifact offset of a member the author types after `.`,
/// `?.`, `!.`, or `:`. A guarded access and an `await` receiver lower
/// to text the compiler wrote, so the member has no offset of its own;
/// the member the lowering wrote is the one the analyzer can read.
fn member_offset(source: &str, check: &str, offset: usize) -> Option<usize> {
    let (base, access, sep, prefix) = context::member_at(source, offset)?;
    // The emit keeps every line, so the member sits on the same one.
    let line = source.get(..offset)?.matches('\n').count();
    let mut start = 0;
    let mut source_start = 0;

    for _ in 0..line {
        start += check.get(start..)?.find('\n')? + 1;
        source_start += source.get(source_start..)?.find('\n')? + 1;
    }

    let end = check
        .get(start..)?
        .find('\n')
        .map_or(check.len(), |i| start + i);
    let source_end = source
        .get(source_start..)?
        .find('\n')
        .map_or(source.len(), |i| source_start + i);
    let column = context::member_column(
        source.get(source_start..source_end)?,
        check.get(start..end)?,
        &base,
        access,
        sep,
        prefix,
        offset - source_start,
    )
    .or_else(|| {
        // `xs[1]?.m` binds the index to a name of the lowering's own,
        // so the receiver of the source is not on the lowered line.
        context::guarded_member_column(
            source.get(source_start..offset)?,
            check.get(start..end)?,
            sep,
        )
    })?;

    Some(start + column)
}

/// Whether the map's own answer already puts the offset right after the
/// same access. The emit copies most of them, and moving one that
/// landed right would cost the member list it already answers.
fn lands_on_member(source: &str, check: &str, offset: usize, mapped: usize) -> bool {
    let Some((base, access, sep, prefix)) = context::member_at(source, offset) else {
        return true;
    };

    if access != context::Access::Plain {
        return false;
    }

    let head = &check[..mapped.min(check.len())];
    let head = &head[..head.len() - prefix.min(head.len())];
    let receiver = base.rsplit('.').next().unwrap_or(&base);

    head.strip_suffix(sep)
        .is_some_and(|h| h.ends_with(receiver))
}

/// A byte offset in the source as one in the check artifact, or -1 for
/// a byte the emit dropped.
#[wasm_bindgen]
pub fn to_check(source_offset: u32) -> i32 {
    SESSION.with(|s| {
        let s = s.borrow();
        let Some(out) = s.output.as_ref() else {
            return -1;
        };
        // The map answers a plain access the emit copied; the member
        // offset answers the rest.
        let at = match out.map.to_output(source_offset).map(|o| o as usize) {
            Some(m) if lands_on_member(&s.source, &out.check, source_offset as usize, m) => Some(m),

            other => member_offset(&s.source, &out.check, source_offset as usize).or(other),
        };

        at.map_or(-1, |o| i32::try_from(o).unwrap_or(-1))
    })
}

/// Whether a byte of the check artifact is text the compiler wrote.
#[wasm_bindgen]
pub fn generated_at(check_offset: u32) -> bool {
    SESSION.with(|s| {
        s.borrow()
            .output
            .as_ref()
            .is_some_and(|o| o.map.is_generated(check_offset))
    })
}

/// The completion at a byte offset: the items Alloy answers itself, and
/// whether the analyzer's list belongs beside them. The editor filters
/// by the word at the cursor; `from` is where each item's text starts.
#[wasm_bindgen]
pub fn complete(offset: u32) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let source = &s.source;
        let offset = (offset as usize).min(source.len());
        let mut items = Vec::new();
        let word = |label: &str, kind: &str, doc_text: Option<String>, from: usize| {
            json!({ "label": label, "kind": kind, "doc": doc_text, "from": from })
        };

        // A comment: the directives.
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);

        if source[line_start..offset].contains("--") {
            let head = &source[line_start..offset];
            let from = head.rfind("--").map_or(line_start, |i| line_start + i);

            for (name, what) in [
                ("--@alloy-ignore", "Silences the next line that holds code, or this line at its end. Text after the name is the reason."),
                ("--@alloy-ignore-start", "Opens a silent region, up to `--@alloy-ignore-end`. A name after it limits the region to that lint or kind."),
                ("--@alloy-ignore-end", "Closes the innermost `--@alloy-ignore-start`."),
                ("--@alloy-expect-error", "Silences the next line, and is an error when that line has none. Text after the name is the reason."),
                ("--@alloy-nocheck", "Silences every diagnostic in this file."),
                ("--@alloy-lint", "Sets a lint's level for this file: `--@alloy-lint raw_require=allow`."),
                ("--@alloy-preserve", "`alloy flux --fix` writes no rewrite on that line."),
                ("--!strict", "The checker's strict mode for this file."),
                ("--!nonstrict", "The checker's nonstrict mode for this file."),
                ("--!nocheck", "No type checking for this file."),
            ] {
                items.push(word(name, "directive", Some(what.to_string()), from));
            }

            return json!({ "items": items, "luau": false }).to_string();
        }

        if declares_a_name_at(source, offset) {
            return json!({ "items": items, "luau": false }).to_string();
        }

        let Some(ctx) = context::detect(source, offset) else {
            // A member list holds members: no keyword after `.` or `:`.
            let start = word_start(source, offset);

            if start > 0 && matches!(source.as_bytes()[start - 1], b'.' | b':') {
                return json!({ "items": items, "luau": true }).to_string();
            }

            // The analyzer answers nothing inside an `if` expression and
            // right before a literal, which is where an arm, a ternary,
            // and a `default` land. Alloy names the scope for those.
            if context::expression_start(source, offset) {
                items.extend(value_scope(source, offset, &s.decls));
            }

            // The analyzer lists the names; Alloy adds its keywords.
            for k in keywords::ALLOY_KEYWORDS {
                let mut item = word(k, "keyword", keywords::doc(k).map(str::to_string), word_start(source, offset));

                // `case` is half an arm: the list opens again behind it
                // for the pattern.
                if *k == "case" {
                    item["insert"] = json!("case ");
                    item["suggest"] = json!(true);
                }

                items.push(item);
            }

            return json!({ "items": items, "luau": true }).to_string();
        };

        match &ctx {
            Context::Attribute { sigil, target, .. } => {
                let fits = |targets: &[&str]| target.is_none_or(|t| targets.contains(&t));

                for key in keywords::keys_with_prefix("@") {
                    if fits(builtin_attribute_targets(key)) {
                        items.push(word(key, "attribute", keywords::doc(key).map(str::to_string), *sigil));
                    }
                }

                for d in &s.decls {
                    if d.name.starts_with('@') {
                        items.push(word(&d.name, "attribute", Some(d.hover.clone()), *sigil));
                    }
                }
            }

            Context::Macro { sigil, .. } => {
                for key in keywords::keys_with_prefix("$") {
                    items.push(word(key, "macro", keywords::doc(key).map(str::to_string), *sigil));
                }

                for d in &s.decls {
                    if d.name.starts_with('$') {
                        items.push(word(&d.name, "macro", Some(d.hover.clone()), *sigil));
                    }
                }
            }

            // An entry of an attribute argument: the declared type of the
            // parameter says what fits. A union of string literals offers
            // its members and an enum its variants; nothing else belongs,
            // since an argument is a literal.
            Context::AttributeArg { prefix, attr, param, quote } => {
                let from = offset - prefix.len();
                let key = format!("@{attr}");
                let params = s
                    .decls
                    .iter()
                    .find(|d| d.name == key)
                    .map(|d| alloy::declarations::attribute_params(&d.hover))
                    .unwrap_or_default();
                let ty = match param {
                    Some(name) => params.iter().find(|(p, _)| p == name).map(|(_, t)| t),

                    None => params.first().map(|(_, t)| t),
                };

                if let Some(ty) = ty {
                    let element = alloy::desugar::element_type(ty);
                    let detail = format!("takes `{element}` for `@{attr}`");

                    if alloy::desugar::is_string_union(&element) {
                        for part in element.split('|') {
                            let text = part.trim().trim_matches(['"', '\'']);

                            if text.is_empty() {
                                continue;
                            }

                            let mut item = word(text, "constant", Some(detail.clone()), from);
                            item["insert"] = json!(match quote {
                                Some(_) => text.to_string(),

                                None => format!("\"{text}\""),
                            });
                            items.push(item);
                        }
                    } else if quote.is_none() {
                        for shape in &s.shapes {
                            let Shape::Enum { name, variants } = shape else {
                                continue;
                            };

                            if *name != element {
                                continue;
                            }

                            for (v, _) in variants {
                                let path = format!("{name}.{v}");
                                let mut item = word(&path, "constant", Some(detail.clone()), from);
                                item["insert"] = json!(path);
                                items.push(item);
                            }
                        }
                    }
                }
            }

            Context::DeriveArg { prefix } => {
                for key in keywords::keys_with_prefix("derive:") {
                    let name = &key["derive:".len()..];
                    items.push(word(name, "constant", keywords::doc(key).map(str::to_string), offset - prefix.len()));
                }
            }

            Context::CfgArg { prefix } => {
                let from = offset - prefix.len();

                for (name, what) in [
                    ("server", "RunService:IsServer()"),
                    ("client", "RunService:IsClient()"),
                    ("studio", "RunService:IsStudio()"),
                    ("edit", "RunService:IsEdit()"),
                    ("running", "RunService:IsRunning()"),
                    ("test", "an `alloy test` run"),
                ] {
                    items.push(word(name, "constant", Some(format!("`@cfg({name})` holds under {what}.")), from));
                }

                for (name, what) in [
                    ("not", "the condition after it fails"),
                    ("and", "both hold"),
                    ("or", "either holds"),
                    ("any(", "any of the list holds"),
                    ("all(", "all of the list hold"),
                ] {
                    items.push(word(name, "keyword", Some(format!("`{name}`: {what}.")), from));
                }
            }

            Context::RemoteSide { prefix, after } => {
                let from = offset - prefix.len();
                let sides: Vec<(&str, &str)> = match after.as_deref() {
                    None => vec![
                        ("client", "The client fires it; the server handles it."),
                        ("server", "The server fires it; the client handles it."),
                    ],
                    Some("client ") | Some("server ") => vec![("or", "Either side fires it: `client or server`.")],
                    Some("client or") => vec![("server", "Either side fires it, and either side handles it.")],
                    Some("server or") => vec![("client", "Either side fires it, and either side handles it.")],
                    _ => Vec::new(),
                };

                for (side, what) in sides {
                    items.push(word(side, "keyword", Some(what.to_string()), from));
                }
            }

            Context::RemoteFrom { prefix } => {
                items.push(word("from", "keyword", Some("The side that fires the remote: `from client`, `from server`, or `from client or server`.".to_string()), offset - prefix.len()));
            }

            // The playground has one file, so no module answers a spec.
            Context::ImportHead { prefix, type_only, .. } => {
                let from = offset - prefix.len();

                if !*type_only {
                    items.push(word("type", "keyword", Some("A type-only import: it costs nothing at runtime.".to_string()), from));
                    items.push(word("* as", "keyword", Some("The whole module under one name.".to_string()), from));
                }

                items.push(word("{", "keyword", Some("Named exports, one or more, `as` to rename.".to_string()), from));
            }

            // `import { | } from "@game"`: the names in braces are the
            // Roblox services. The playground has one file, so no
            // module answers any other spec.
            Context::ImportNames { prefix, after_name, spec, type_only, sigil } => {
                let from = offset - prefix.len();

                // `import { @|`: an attribute of the module named, and
                // the playground names none.
                if *sigil {
                    return json!({ "items": items, "luau": false }).to_string();
                }

                if *after_name {
                    items.push(word("as", "keyword", Some("Renames the import.".to_string()), from));

                    return json!({ "items": items, "luau": false }).to_string();
                }

                let every_service = matches!(
                    spec.as_deref().and_then(alloy::game_import::game_path),
                    Some(alloy::game_import::GamePath::Every)
                );

                if every_service && !*type_only {
                    for name in alloy::roblox_services::SERVICES {
                        let mut item = word(name, "class", Some(alloy::game_import::service_summary(name)), from);
                        item["detail"] = json!(format!("game:GetService(\"{name}\")"));
                        items.push(item);
                    }
                }
            }

            // `new Instance("Part") { |`: the class's own properties.
            Context::InstanceField { prefix, class } => {
                let from = offset - prefix.len();

                for name in alloy::luaux::roblox::properties(class) {
                    let mut item = word(name, "field", None, from);
                    item["detail"] = json!(format!("property of {class}"));
                    item["insert"] = json!(format!("{name} = ${{1:{name}}}"));
                    items.push(item);
                }
            }

            Context::DestroyAfter { prefix } => {
                items.push(word("after", "keyword", Some("`destroy x after n` waits `n` seconds, then destroys `x`.".to_string()), offset - prefix.len()));
            }

            Context::AfterDo { prefix, filtered } => {
                let from = offset - prefix.len();
                items.push(word("do", "keyword", Some("Opens the block the timer runs.".to_string()), from));

                if !filtered {
                    items.push(word("where", "keyword", Some("A condition on the block, read when the timer fires.".to_string()), from));
                }
            }

            Context::ImportStar => {
                items.push(word("as", "keyword", Some("The name the module takes here.".to_string()), offset));
            }

            // A default binding took the first slot; the braces follow.
            Context::ImportBrace => {
                items.push(word("{", "keyword", Some("Named exports, one or more, `as` to rename.".to_string()), offset));
            }

            Context::ImportFrom => {
                items.push(word("from", "keyword", Some("The module path, as a string.".to_string()), offset));
            }

            Context::DeclarationAs { prefix, interface } => {
                let from = offset - prefix.len();
                items.push(word("as", "keyword", Some("Opens the body: the fields of a struct, the variants of an enum, the methods of an `impl` or a `trait`.".to_string()), from));

                if *interface {
                    items.push(word("extends", "keyword", Some("The interfaces this one takes its fields from.".to_string()), from));
                }
            }

            Context::EnumPayload { prefix } => {
                let from = offset - prefix.len();

                for name in ["number", "string", "boolean", "any", "unknown", "nil", "thread", "buffer"] {
                    items.push(word(name, "type", None, from));
                }

                let folded = folded_names(source);

                for d in &s.decls {
                    if !d.name.starts_with(['@', '$']) && !d.name.contains('.') && !folded.contains(&d.name) && d.hover.contains("```alloy\n") && (d.hover.contains("struct ") || d.hover.contains("enum ") || d.hover.contains("interface ") || d.hover.contains("type ")) {
                        items.push(word(&d.name, "type", Some(d.hover.clone()), from));
                    }
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }
            }

            Context::AttributeOn => {
                items.push(word("on", "keyword", Some("What the attribute goes on: `on struct, field`.".to_string()), offset));
            }

            Context::AttributeTarget { prefix } => {
                let from = offset - prefix.len();

                for name in ["function", "struct", "enum", "variant", "field", "param", "remote", "interface", "type", "local"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::TypeSlot { prefix, prefers } => {
                let from = offset - prefix.len();

                for name in ["number", "string", "boolean", "any", "unknown", "nil", "thread", "buffer"] {
                    items.push(word(name, "type", None, from));
                }

                // `extends` and the trait of an `impl` want a contract;
                // `impl X` and the target after `for` want a struct or
                // an enum. The rest of the list stays, one rank down.
                let folded = folded_names(source);

                for d in &s.decls {
                    let head = d.hover.lines().nth(1).unwrap_or("");
                    let concrete = head.contains("struct ") || head.contains("enum ");
                    let contract = head.contains("interface ") || head.contains("trait ");

                    if !d.name.starts_with(['@', '$']) && !d.name.contains('.') && !folded.contains(&d.name) && (concrete || contract || head.contains("type ")) {
                        let ranks = match prefers {
                            context::Prefers::Any => false,
                            context::Prefers::Contract => contract,
                            context::Prefers::Concrete => concrete,
                        };
                        let mut item = word(&d.name, "type", Some(d.hover.clone()), from);
                        item["sort"] = json!(if ranks { 0 } else { 1 });
                        items.push(item);
                    }
                }

                for name in ["Array", "HashMap", "Set", "Queue", "Heap", "Scope", "Iter", "Result", "Future", "Signal", "Partial", "Readonly", "Sink"] {
                    items.push(word(name, "type", keywords::doc(name).map(str::to_string), from));
                }

                // The traits a bound and an `impl` take.
                for name in ["Display", "Debug", "Clone", "Eq", "PartialEq", "Ord", "Serialize", "Deletable", "Add", "Sub", "Mul", "Div"] {
                    items.push(word(name, "type", keywords::doc(name).map(str::to_string), from));
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }

                // A namespace is no type itself, and `Shapes.Box` is
                // one. The slot offers the name as the head of that
                // path, and the accept writes the `.` too.
                for name in namespace_prefixes(source, &s.decls) {
                    let mut item = word(&name, "module", Some(format!("```alloy\nnamespace {name}\n```")), from);
                    item["insert"] = json!(format!("{name}."));
                    items.push(item);
                }
            }

            Context::NewTarget { prefix } => {
                let from = offset - prefix.len();

                for d in &s.decls {
                    let head = d.hover.lines().nth(1).unwrap_or("");

                    if head.contains("struct ") {
                        items.push(word(&d.name, "class", Some(d.hover.clone()), from));
                    }
                }

                for name in ["HashMap", "Set", "Queue", "Heap", "Scope", "Signal", "Symbol", "Array"] {
                    items.push(word(name, "class", keywords::doc(name).map(str::to_string), from));
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }
            }

            Context::MatchCase { prefix, scrutinee } => {
                let from = offset - prefix.len();
                let kind = scrutinee.as_deref().map_or(MatchKind::Unknown, |t| match_kind(source, offset, t, &s.decls));

                match &kind {
                    MatchKind::Enum(name) => {
                        for d in &s.decls {
                            if let Some(variant) = d.name.strip_prefix(name.as_str()).and_then(|r| r.strip_prefix('.')) {
                                let mut item = word(variant, "constant", Some(d.hover.clone()), from);
                                item["insert"] = json!(variant_insert(variant, d.hover.lines().nth(1).unwrap_or("")));
                                items.push(item);
                            }
                        }
                    }

                    MatchKind::Result => {
                        for (name, insert) in [("Ok", "Ok(${1:v})"), ("Err", "Err(${1:e})")] {
                            let mut item = word(name, "constant", None, from);
                            item["insert"] = json!(insert);
                            items.push(item);
                        }
                    }

                    MatchKind::Array => {
                        for (name, insert) in [("[ first, ...rest ]", "[ ${1:first}, ...${2:rest} ]"), ("[ ]", "[ ]")] {
                            let mut item = word(name, "constant", None, from);
                            item["insert"] = json!(insert);
                            items.push(item);
                        }
                    }

                    // A string or a number matches its own literals.
                    MatchKind::Literal => {}

                    MatchKind::Unknown => {
                        for d in &s.decls {
                            if let Some((_, variant)) = d.name.split_once('.') {
                                items.push(word(variant, "constant", Some(d.hover.clone()), from));
                            }
                        }

                        for name in ["Ok", "Err", "Enum", "_"] {
                            items.push(word(name, "keyword", None, from));
                        }
                    }
                }

                items.push(word("default", "keyword", None, from));
            }

            Context::FieldStart { prefix } => {
                let from = offset - prefix.len();

                for name in ["read", "write", "private", "public", "end"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::MemberStart { prefix } => {
                let from = offset - prefix.len();

                for name in ["function", "async function", "private function", "public", "end"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            // A variant name is the author's own; the `end` closes the body.
            Context::VariantStart { prefix } => {
                items.push(word("end", "keyword", None, offset - prefix.len()));
            }

            // `new Instance("|")`: the classes the engine builds.
            Context::ClassName { prefix } => {
                let from = offset - prefix.len();

                for name in alloy::luaux::roblox::creatable_classes() {
                    let mut item = word(name, "class", None, from);
                    item["detail"] = json!("Roblox class");
                    items.push(item);
                }
            }

            // A line of an attribute contract. The context holds
            // exactly the words that fit, so the list is that alone.
            Context::ContractClause { prefix, words } => {
                let from = offset - prefix.len();

                for w in words {
                    items.push(word(w, "keyword", None, from));
                }
            }

            // A trait declares a contract; every method in it is public.
            Context::TraitMemberStart { prefix } => {
                let from = offset - prefix.len();

                for name in ["function", "async function", "end"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::StructField { prefix, target } => {
                let from = offset - prefix.len();
                let inside = context::impl_target(source, offset).as_deref() == Some(target.as_str());
                let body = s.decls.iter().find(|d| d.name == *target).map(|d| d.hover.clone());

                for field in body.map(|h| context::record_entries(&h)).unwrap_or_default() {
                    if !inside && field.private {
                        continue;
                    }

                    let mut item = word(&field.name, "field", Some(format!("A field of `{target}`.")), from);
                    item["detail"] = json!(format!("{}: {}", field.name, field.ty));
                    item["insert"] = json!(format!("{} = ${{1:{}}}", field.name, field.name));
                    items.push(item);
                }
            }

            // `profile["|`: the keys the receiver's type names. An
            // array and a `{ [string]: T }` name none, and the empty
            // list there hands the position back to the analyzer.
            Context::IndexKey { prefix, receiver, quote } => {
                let from = offset - prefix.len() - quote.map_or(0, char::len_utf8);
                let q = quote.unwrap_or('"');
                let ty = match context::declared(source, offset, receiver) {
                    Some(context::Declared::Annotation(t)) => t,

                    _ => String::new(),
                };
                let ty = ty.trim().trim_end_matches('?').trim();
                let body = match ty.starts_with('{') {
                    true => Some(ty.to_string()),

                    false => s.decls.iter().find(|d| d.name == ty).map(|d| d.hover.clone()),
                };

                for field in body.map(|h| context::record_entries(&h)).unwrap_or_default() {
                    let label = format!("{q}{}{q}", field.name);
                    let mut item = word(&label, "field", Some(format!("A key of `{receiver}`.")), from);
                    item["detail"] = json!(field.ty);
                    items.push(item);
                }
            }

            // The path list of a one-file playground: the `@game` alias
            // alone, and the services under it.
            Context::ImportSpec { text, start } => {
                let head = text
                    .strip_prefix("@game/")
                    .filter(|rest| !rest.contains('/'))
                    .map(|_| "@game/");

                match head {
                    Some(head) => {
                        let from = start + head.len();

                        for name in alloy::roblox_services::SERVICES {
                            let mut item = word(name, "class", Some(alloy::game_import::service_summary(name)), from);
                            item["detail"] = json!(format!("game:GetService(\"{name}\")"));
                            items.push(item);
                        }
                    }

                    None => {
                        if text.is_empty() {
                            items.push(word("@game", "class", Some("The Roblox services, in braces: `import { Players } from \"@game\"`.".to_string()), *start));
                            items.push(word("@game/", "module", Some("One Roblox service: `import Players from \"@game/Players\"`.".to_string()), *start));
                        }
                    }
                }
            }

            Context::Nothing => {}
        }

        json!({ "items": items, "luau": false }).to_string()
    })
}

/// The hover Alloy answers itself at a byte offset: a keyword, an
/// operator, or a declaration of the file. `null` when the analyzer's
/// type is the answer.
#[wasm_bindgen]
pub fn hover(offset: u32) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let source = &s.source;
        let offset = (offset as usize).min(source.len());

        if let Some((start, end, text)) = keywords::hover(source, offset) {
            // A std type: the overview, then the names a reader can
            // hover on their own.
            let text =
                alloy::docs::type_markdown(&source[start..end]).unwrap_or_else(|| text.to_string());

            return json!({ "from": start, "to": end, "markdown": text }).to_string();
        }

        // A std member: the member's own section, not the type's page.
        if let Some((key, m)) = std_member_at(source, offset) {
            let (start, end) = keywords::word_range(source, offset);
            let markdown = alloy::docs::member_hover(key, m);

            return json!({ "from": start, "to": end, "markdown": markdown }).to_string();
        }

        if keywords::is_word_at(source, offset) {
            let (start, end) = keywords::word_range(source, offset);
            let name = &source[start..end];

            if let Some(d) = s
                .decls
                .iter()
                .find(|d| d.name == name || d.name.trim_start_matches(['@', '$']) == name)
            {
                return json!({ "from": start, "to": end, "markdown": d.hover }).to_string();
            }

            if let Some(text) = keywords::doc(name)
                && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            {
                return json!({ "from": start, "to": end, "markdown": text }).to_string();
            }
        }

        "null".to_string()
    })
}

/// A type the analyzer printed, as the source reads it: the runtime's
/// prefix goes, and a struct or a collection reads by name.
#[wasm_bindgen]
pub fn fold(text: &str) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let known = shapes::Known {
            shapes: s.shapes.clone(),
            interfaces: s.interfaces.clone(),
            namespaces: alloy::declarations::namespace_names(&s.source),
            tables: alloy::tables::plain_tables(&s.source),
        };
        let mut out = text.to_string();

        for primitive in alloy::desugar::PRIMITIVES {
            out = out.replace(&format!("__alloy_{primitive}."), &format!("{primitive}."));
        }

        let out = out
            .replace("__alloy.", "")
            .replace("__mapped_optional<", "Partial<")
            .replace("__mapped_read<", "Readonly<")
            .replace("__mapped_write<", "Sink<")
            .replace("Future<nil>", "Future<()>");

        shapes::fold(&out, &known)
    })
}

/// The documentation of a std name or a keyword, for a hover the
/// analyzer answered with a type. `HashMap:get` and `HashMap.get` name
/// one member; a std type name lists its members under the overview.
#[wasm_bindgen]
pub fn doc_of(name: &str) -> String {
    if let Some((key, m)) = alloy::docs::split_member(name) {
        return alloy::docs::member_hover(key, m);
    }

    if let Some(text) = alloy::docs::type_markdown(name) {
        return text;
    }

    keywords::doc(name).unwrap_or("").to_string()
}

/// The std member the byte sits on: the word after a `.` or a `:` whose
/// receiver resolves to a std type that documents it.
fn std_member_at(
    source: &str,
    offset: usize,
) -> Option<(&'static str, &'static alloy::docs::Member)> {
    let (name, sigil, receiver) = alloy::docs::member_spot(source, offset)?;

    if receiver.is_empty() {
        return None;
    }

    let (key, on_type) = match alloy::docs::member_owner(receiver) {
        Some(key) => (key, true),

        None => {
            let base = match context::declared(source, sigil, receiver)? {
                context::Declared::Annotation(t) => alloy::docs::type_head(&t),
                context::Declared::Init(v) => alloy::docs::value_head(&v),
            }?;

            (alloy::docs::member_owner(&base)?, false)
        }
    };
    let m = alloy::docs::member(key, name)?;

    alloy::docs::member_fits(m.kind, on_type).then_some((key, m))
}

/// What a `match` scrutinee resolves to, which decides the arms.
enum MatchKind {
    /// An enum in scope: its variants are the arms.
    Enum(String),
    /// A `Result<T, E>`: `Ok` and `Err`.
    Result,
    /// `T[]` or `Array<T>`: the array patterns.
    Array,
    /// A string or a number: only `default` fits.
    Literal,
    /// Nothing the playground reads.
    Unknown,
}

/// The type a `match` scrutinee has. A plain name resolves from its
/// annotation, from the variant it starts at, or from a declaration.
fn match_kind(source: &str, offset: usize, scrutinee: &str, decls: &[Declaration]) -> MatchKind {
    let name = scrutinee.trim();

    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return MatchKind::Unknown;
    }

    // `self` in an `impl` body is the type the block is for.
    if name == "self" {
        return context::impl_target(source, offset)
            .map_or(MatchKind::Unknown, |t| kind_of_type(&t, decls));
    }

    match context::declared(source, offset, name) {
        Some(context::Declared::Annotation(t)) => kind_of_type(&t, decls),

        Some(context::Declared::Init(v)) => kind_of_value(&v, decls),

        None => kind_of_name(name, decls),
    }
}

fn kind_of_type(text: &str, decls: &[Declaration]) -> MatchKind {
    let t = text.trim().trim_end_matches('?').trim();

    if t == "Result" || t.starts_with("Result<") {
        return MatchKind::Result;
    }

    if t.ends_with("[]") || t.starts_with("Array<") {
        return MatchKind::Array;
    }

    if matches!(t, "string" | "number") {
        return MatchKind::Literal;
    }

    kind_of_name(t.split('<').next().unwrap_or(t).trim(), decls)
}

fn kind_of_name(name: &str, decls: &[Declaration]) -> MatchKind {
    let Some(d) = decls.iter().find(|d| d.name == name) else {
        return MatchKind::Unknown;
    };
    let head = d.hover.lines().nth(1).unwrap_or("");

    if head.contains("enum ") {
        return MatchKind::Enum(name.to_string());
    }

    if head.contains("Result<") {
        return MatchKind::Result;
    }

    if head.contains("[]") || head.contains("Array<") {
        return MatchKind::Array;
    }

    MatchKind::Unknown
}

fn kind_of_value(text: &str, decls: &[Declaration]) -> MatchKind {
    let t = text.trim();
    let head: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if matches!(head.as_str(), "Ok" | "Err") {
        return MatchKind::Result;
    }

    if head.is_empty() || !t[head.len()..].starts_with('.') {
        return MatchKind::Unknown;
    }

    match kind_of_name(&head, decls) {
        MatchKind::Enum(name) => MatchKind::Enum(name),

        _ => MatchKind::Unknown,
    }
}

/// A variant inserts its name, and opens a payload slot when it takes
/// one.
fn variant_insert(variant: &str, signature: &str) -> String {
    let payload = match signature.find('(') {
        Some(open) if !signature[open + 1..].trim_start().starts_with(')') => {
            let close = signature[open..]
                .find(')')
                .map_or(signature.len(), |i| open + i);

            signature[open + 1..close].split(',').count()
        }

        _ => 0,
    };

    match payload {
        0 => variant.to_string(),

        // One tab stop per value the variant carries.
        n => {
            let slots: Vec<String> = (1..=n).map(|i| format!("${i}")).collect();

            format!("{variant}({})", slots.join(", "))
        }
    }
}

/// The names an expression at the caret may write: the locals and the
/// parameters in scope, the structs and the enums the file declares,
/// the std names, and the words an expression takes.
fn value_scope(source: &str, offset: usize, decls: &[Declaration]) -> Vec<Value> {
    let from = word_start(source, offset);
    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |label: &str, kind: &str, doc_text: Option<String>, detail: &str| {
        if !seen.insert(label.to_string()) {
            return;
        }

        items.push(json!({
            "label": label,
            "kind": kind,
            "doc": doc_text,
            "from": from,
            "detail": detail,
        }));
    };

    for local in context::locals_in_scope(source, offset) {
        let kind = match local.kind {
            context::LocalKind::Function => "function",

            _ => "variable",
        };
        let detail = local
            .annotation
            .clone()
            .unwrap_or_else(|| match local.kind {
                context::LocalKind::Parameter => "parameter".to_string(),

                _ => "local".to_string(),
            });
        push(&local.name, kind, None, &detail);
    }

    // An interface, a trait, and a type alias name a type, not a value.
    for d in decls {
        if d.name.starts_with(['@', '$']) || d.name.contains('.') {
            continue;
        }

        let head = d.hover.lines().nth(1).unwrap_or("");
        let kind = if head.contains("struct ") || head.contains("class ") {
            "class"
        } else if head.contains("enum ") {
            "enum"
        } else {
            continue;
        };
        push(&d.name, kind, Some(d.hover.clone()), "alloy");
    }

    for name in alloy::desugar::AMBIENT {
        push(
            name,
            "class",
            keywords::doc(name).map(str::to_string),
            "alloy:std",
        );
    }

    for name in EXPRESSION_GLOBALS {
        push(name, "variable", None, "roblox");
    }

    for name in [
        "if", "not", "new", "await", "try", "function", "true", "false", "nil",
    ] {
        push(
            name,
            "keyword",
            keywords::doc(name).map(str::to_string),
            "keyword",
        );
    }

    items
}

/// The flat names the emit writes for the members of a namespace:
/// `Shapes_Box` for `namespace Shapes as export type Box`. The
/// declaration index holds them so a hover on the artifact reads, and
/// no list offers one.
fn folded_names(source: &str) -> Vec<String> {
    alloy::declarations::namespace_names(source)
        .into_iter()
        .map(|(emitted, _)| emitted)
        .collect()
}

/// The namespaces a bare type slot offers as the head of a path:
/// `Shapes` in front of `Shapes.Box`. A namespace of functions alone
/// reaches no type, so it stays out.
fn namespace_prefixes(source: &str, decls: &[Declaration]) -> Vec<String> {
    const TYPE_WORDS: [&str; 5] = ["struct ", "enum ", "trait ", "interface ", "type "];

    let mut out: Vec<String> = Vec::new();

    for (_, path) in alloy::declarations::namespace_names(source) {
        let Some((head, _)) = path.split_once('.') else {
            continue;
        };

        if out.iter().any(|n| n == head) {
            continue;
        }

        let holds = decls.iter().any(|d| {
            d.name == path
                && d.hover
                    .lines()
                    .nth(1)
                    .is_some_and(|l| TYPE_WORDS.iter().any(|w| l.contains(w)))
        });

        if holds {
            out.push(head.to_string());
        }
    }

    out.sort();
    out
}

fn word_start(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut start = offset;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    start
}

#[cfg(test)]
mod tests {
    /// The playground offers every directive the compiler reads, and
    /// re-levels a lint the file's own `--@alloy-lint` names.
    #[test]
    fn the_directive_list_and_the_lint_levels_reach_the_playground() {
        let source = "--@alloy-lint raw_require=allow\nlocal m = require(\"./m\")\n--@\n";
        let out = super::set_source(source);
        assert!(!out.contains("\"name\":\"raw_require\""), "{out}");

        let at = source.find("--@").map(|i| i + 3).unwrap() as u32;
        let items = super::complete(at);

        for name in alloy::directives::NAMES {
            assert!(items.contains(name), "`{name}` is not offered: {items}");
        }
    }

    /// The playground offers the same three service positions the
    /// editor does: the path list, the segment after `@game/`, and the
    /// names in the braces of `from "@game"`.
    #[test]
    fn the_service_positions_reach_the_playground() {
        let labels = |source: &str, at: usize| -> Vec<String> {
            super::set_source(source);
            let items: serde_json::Value =
                serde_json::from_str(&super::complete(at as u32)).expect("items");

            items["items"]
                .as_array()
                .expect("a list")
                .iter()
                .filter_map(|i| i["label"].as_str().map(str::to_string))
                .collect()
        };

        let path = "import P from \"\"\n";
        let at = path.rfind('"').expect("the quote");
        let list = labels(path, at);
        assert!(list.contains(&"@game".to_string()), "{list:?}");
        assert!(list.contains(&"@game/".to_string()), "{list:?}");
        assert!(!list.contains(&"game:".to_string()), "{list:?}");

        let one = "import P from \"@game/\"\n";
        let at = one.rfind('"').expect("the quote");
        assert!(labels(one, at).contains(&"Players".to_string()));

        let braces = "import {  } from \"@game\"\n";
        let at = braces.find('{').expect("the brace") + 2;
        let names = labels(braces, at);
        assert!(
            names.contains(&"ReplicatedStorage".to_string()),
            "{names:?}"
        );
        assert!(!names.contains(&"type".to_string()), "{names:?}");
    }

    /// A type slot offers the namespace as the head of a path, and
    /// never the flat name the emit writes for a member of it.
    #[test]
    fn a_type_slot_takes_a_namespace_and_no_folded_name() {
        let source = concat!(
            "namespace Shapes as\n",
            "    export type Box = { w: number }\n",
            "end\n",
            "namespace Funcs as\n",
            "    export function go() end\n",
            "end\n",
            "struct P as\n",
            "    stor: \n",
            "end\n",
        );
        super::set_source(source);

        let at = source.find("stor: ").expect("the slot") + "stor: ".len();
        let items: serde_json::Value =
            serde_json::from_str(&super::complete(at as u32)).expect("items");
        let rows = items["items"].as_array().expect("a list");
        let row = |name: &str| rows.iter().find(|i| i["label"] == name).cloned();

        let shapes = row("Shapes").expect("the namespace");
        assert_eq!(shapes["insert"], "Shapes.");
        assert_eq!(shapes["kind"], "module");

        // A namespace of functions alone reaches no type.
        assert!(row("Funcs").is_none());
        // `Shapes_Box` is the emit's name, and `Shapes.Box` is a path,
        // not a name the word at the caret filters.
        assert!(row("Shapes_Box").is_none());
        assert!(row("Shapes.Box").is_none());
    }

    /// A hover on a std member answers with the member's own section,
    /// and `doc_of` takes the qualified name the analyzer has.
    #[test]
    fn a_std_member_hovers_as_its_own_section() {
        let source = "local prices: HashMap<string, number> = HashMap.new()\nlocal price = prices:get(\"a\")\n";
        super::set_source(source);
        let at = source.find(":get").unwrap() + 2;
        let hover: serde_json::Value =
            serde_json::from_str(&super::hover(at as u32)).expect("hover json");

        assert!(
            hover["markdown"]
                .as_str()
                .is_some_and(|m| m.starts_with("**HashMap:get**")),
            "{hover}"
        );
        assert!(super::doc_of("HashMap:get").starts_with("**HashMap:get**"));
        assert!(super::doc_of("HashMap").contains("Members: `new`"));
    }

    /// `bx?.` and `bx?.na`: the analyzer reads the member the lowering
    /// wrote, since the one the author typed is inside generated text.
    #[test]
    fn a_member_after_an_optional_access_maps_into_the_lowering() {
        let source = "local bx: Part? = nil\nlocal deep = bx?.Name\nprint(bx, deep)\n";
        super::set_source(source);
        let dot = source.find("?.").unwrap() + 2;

        for at in [dot, dot + 4] {
            let check = super::to_check(at as u32);
            assert!(check >= 0, "no check offset for {at}");
        }

        // The offset lands right after the `.` the lowering wrote.
        let out = super::set_source(source);
        assert!(out.contains("bx.Name"), "{out}");
    }

    /// `await X.m()` moves the receiver into the call the emit wrote,
    /// and `p!.f` closes its guard before the separator.
    #[test]
    fn an_awaited_and_an_asserted_receiver_map_into_the_lowering() {
        let source = concat!(
            "local async function f()\n",
            "    local s = await Future.all([])\n",
            "end\n"
        );
        super::set_source(source);

        let at = source.find("Future.").unwrap() + "Future.".len();
        let check = super::to_check(at as u32);
        assert!(check >= 0, "no check offset for the awaited receiver");

        let text = super::SESSION.with(|s| {
            s.borrow()
                .output
                .as_ref()
                .map(|o| o.check.clone())
                .unwrap_or_default()
        });
        assert!(
            text[..check as usize].ends_with("Future."),
            "the offset lands past the receiver: {}",
            &text[..check as usize]
        );
    }

    /// A struct literal lists the fields of its struct.
    #[test]
    fn a_struct_literal_lists_its_own_fields() {
        let source = concat!(
            "struct Stats as\n",
            "    health: number\n",
            "    private kills: number\n",
            "end\n",
            "local s = new Stats { \n"
        );
        super::set_source(source);
        let at = source.rfind("{ ").unwrap() + 2;
        let items = super::complete(at as u32);
        assert!(items.contains("health"), "{items}");
        assert!(!items.contains("kills"), "{items}");
        assert!(!items.contains("Workspace"), "{items}");
    }

    /// The arms of an `if` expression get the scope: the analyzer
    /// answers nothing there, so the playground names it itself.
    #[test]
    fn an_if_expression_arm_names_the_scope() {
        let source = concat!(
            "struct Round as\n",
            "    seconds: number\n",
            "end\n",
            "\n",
            "export function pick(acc: number): string\n",
            "    local many = \"many\"\n",
            "    return if acc > 0 then \"a\" else \"b\"\n",
            "end\n",
        );
        super::set_source(source);

        let at = source.find("then \"a\"").unwrap() + "then ".len();
        let items = super::complete(at as u32);

        for name in ["acc", "many", "pick", "Round", "Ok", "print"] {
            assert!(items.contains(&format!("\"{name}\"")), "`{name}`: {items}");
        }

        // The analyzer still gets its turn at the same byte.
        assert!(items.contains("\"luau\":true"), "{items}");
    }

    #[test]
    fn a_preserved_line_carries_no_rewrite() {
        let kept = "--@alloy-preserve\nlocal n = p and p.Name\n";
        let out = super::set_source(kept);
        assert!(out.contains("\"name\":\"manual_safe_access\""), "{out}");
        assert!(!out.contains("player?"), "{out}");
        assert!(!out.contains("\"replacement\""), "{out}");

        let plain = "local n = p and p.Name\n";
        assert!(
            super::set_source(plain).contains("\"replacement\""),
            "the rewrite reaches the playground without the directive"
        );
    }
    /*
    The playground compiles through the same path as the CLI, so its
    problem list is the CLI's.

    A stale wasm build drew five `ReservedWord` errors over
    `local new = 1` and two more over a mis-scoped `try do`. The page
    ships a binary, so only a test in this crate says whether the code
    behind it agrees with the compiler.
    */
    #[test]
    fn the_playground_reports_what_the_compiler_reports() {
        let names = "local new = 1\nlocal match = 2\nlocal struct = 3\nprint(new, match, struct)\n";
        let out = super::set_source(names);
        assert!(!out.contains("reserved word"), "{out}");
        assert!(out.contains("\"diagnostics\":[]"), "{out}");

        // A `try do` block as a bare statement: one report, the same one
        // `alloy flux` gives.
        let try_do = concat!(
            "local function risky(): number\n",
            "    return 1\n",
            "end\n",
            "\n",
            "try do\n",
            "    local v = risky()!\n",
            "    print(v)\n",
            "end\n",
        );
        let out = super::set_source(try_do);
        let value: serde_json::Value = serde_json::from_str(&out).expect("json");
        let diagnostics = value["diagnostics"].as_array().expect("diagnostics");
        assert_eq!(diagnostics.len(), 1, "{out}");
        assert_eq!(
            diagnostics[0]["message"],
            serde_json::json!("AlloyError: this expression is not a statement")
        );
    }
}
