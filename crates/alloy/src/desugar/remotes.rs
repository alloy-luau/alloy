//! Remote declarations and their wire layout.

use alloy_syntax::ast::RemoteDecl;

use super::types::{group_len, split_top_level};
use super::*;

/// The number widths a parameter or a field may carry.
pub const WIRE_WIDTHS: &[&str] = &["u8", "u16", "u32", "i8", "i16", "i32", "f32", "f64"];

/// The type text a wire width cannot pack. A width packs a number, and
/// `any` crosses as one; a trailing `?` makes no difference. `None` for a
/// type the width fits.
pub(crate) fn width_misfit(ty: &str) -> Option<&str> {
    let base = ty.trim_end_matches('?').trim();

    (base != "number" && base != "any").then_some(base)
}

/// Every wire width written in a run of source, each with the byte it
/// starts at. A remote parameter carries its attributes as text in front
/// of the name, so `@u8 @u16 x` reads as two widths here.
fn widths_in(gap: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut at = 0;

    while let Some(i) = gap[at..].find('@') {
        let start = at + i;
        let word: String = gap[start + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        at = start + 1 + word.len();

        if WIRE_WIDTHS.contains(&word.as_str()) {
            out.push((start as u32, word));
        }
    }

    out
}

/// One node of a wire layout: how a value packs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Wire {
    /// `u8`, `f64`, `bool`, `str`, or `any` for what crosses as it is.
    Scalar { kind: String, optional: bool },
    /// A record, field by field; `struct` names the local whose
    /// metatable the reader restores.
    Table {
        fields: Vec<(String, Wire)>,
        struct_name: Option<String>,
        optional: bool,
    },
    /// An array: a count, then each item.
    Array { item: Box<Wire>, optional: bool },
}

/// The bytes Roblox carries on an UnreliableRemoteEvent.
pub(crate) const UNRELIABLE_LIMIT: usize = 900;

impl Wire {
    /// Why the value has no size bound, or `None` when it has one. A
    /// number, a boolean, and a Roblox datatype all pack to a fixed
    /// width; a string and an array grow with what the caller passes.
    fn unbounded(&self) -> Option<&'static str> {
        match self {
            Wire::Scalar { kind, .. } if kind == "str" => Some("a string has no length bound"),

            Wire::Scalar { .. } => None,

            Wire::Table { fields, .. } => fields.iter().find_map(|(_, w)| w.unbounded()),

            Wire::Array { .. } => Some("an array has no length bound"),
        }
    }

    fn is_any(&self) -> bool {
        matches!(self, Wire::Scalar { kind, .. } if kind == "any")
    }

    /// Every struct name the layout writes, at any depth.
    fn structs(&self) -> Vec<String> {
        match self {
            Wire::Scalar { .. } => Vec::new(),

            Wire::Table {
                fields,
                struct_name,
                ..
            } => struct_name
                .iter()
                .cloned()
                .chain(fields.iter().flat_map(|(_, w)| w.structs()))
                .collect(),

            Wire::Array { item, .. } => item.structs(),
        }
    }

    fn with_optional(mut self, flag: bool) -> Self {
        match &mut self {
            Wire::Scalar { optional, .. }
            | Wire::Table { optional, .. }
            | Wire::Array { optional, .. } => *optional = *optional || flag,
        }

        self
    }

    /// The layout as the Luau table the runtime reads.
    fn luau(&self) -> String {
        match self {
            Wire::Scalar { kind, optional } => {
                luau_string(&format!("{kind}{}", if *optional { "?" } else { "" }))
            }

            Wire::Table {
                fields,
                struct_name,
                optional,
            } => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|(n, w)| format!("{{ {}, {} }}", luau_string(n), w.luau()))
                    .collect();
                let mut out = format!("{{ fields = {{ {} }}", fields.join(", "));

                if let Some(name) = struct_name {
                    out.push_str(&format!(", struct = {name}"));
                }

                if *optional {
                    out.push_str(", optional = true");
                }

                out.push_str(" }");

                out
            }

            Wire::Array { item, optional } => {
                let mut out = format!("{{ item = {}, array = true", item.luau());

                if *optional {
                    out.push_str(", optional = true");
                }

                out.push_str(" }");

                out
            }
        }
    }
}

/// The name and type of one field, as the wire walk reads it.
type Field = (String, String);

/// What one remote parameter cannot carry: the field path that holds it,
/// the type, and why.
type Offender = (Option<String>, String, &'static str);

/// The part of a remote parameter that cannot cross the wire.
/// `fields_of` gives the fields of a struct the file names, since a
/// struct packs down to any depth.
fn offender(
    ty: &str,
    fields_of: &dyn Fn(&str) -> Option<Vec<Field>>,
    depth: usize,
) -> Option<Offender> {
    let mut trimmed = ty.trim();

    while let Some(t) = trimmed
        .strip_suffix('?')
        .or_else(|| trimmed.strip_suffix("[]"))
    {
        trimmed = t.trim();
    }

    // A struct that holds itself, straight or through another, would
    // walk forever.
    if depth > 6 {
        return None;
    }

    let named = match trimmed.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        Some(inner) => split_top_level(inner, ',')
            .iter()
            .filter_map(|part| {
                let (name, value) = part.trim().split_once(':')?;
                let name = name.trim();
                let name = name
                    .strip_prefix("read ")
                    .or_else(|| name.strip_prefix("write "))
                    .unwrap_or(name)
                    .trim();

                Some((name.to_string(), value.to_string()))
            })
            .collect(),

        None => match fields_of(trimmed) {
            Some(fields) => fields,

            // Not a record and no struct the file names: the type
            // itself is the answer.
            None => return not_wire_type(trimmed).map(|why| (None, trimmed.to_string(), why)),
        },
    };

    for (name, value) in named {
        let Some((deeper, bad, why)) = offender(&value, fields_of, depth + 1) else {
            continue;
        };
        let path = match deeper {
            Some(d) => format!("{name}.{d}"),

            None => name,
        };

        return Some((Some(path), bad, why));
    }

    None
}

pub(crate) fn not_wire_type(ty: &str) -> Option<&'static str> {
    if ty.contains("->") {
        return Some("is a function type");
    }

    let words = ty.split(|c: char| !c.is_alphanumeric() && c != '_');

    for w in words {
        match w {
            "thread" => return Some("is a coroutine"),
            "Future" => return Some("is a `Future`, which holds a coroutine"),
            "Signal" => return Some("is a `Signal`, which holds functions"),
            // A remote strips the metatable, so the methods do not
            // arrive. `Array<T>` and `T[]` are the exception: the wire
            // packs the items and the other side builds the array.
            "HashMap" | "Set" | "Queue" | "Heap" | "Iter" => {
                return Some("carries a metatable that the wire cannot pack");
            }
            _ => {}
        }
    }

    None
}

impl<'s> Desugar<'s> {
    /// `wire_offender` with the structs of this file, so a parameter
    /// that names one reports the field that cannot cross the wire.
    fn offender_of(&self, ty: &str) -> Option<Offender> {
        offender(
            ty,
            &|name| {
                let declared = self.struct_wire.get(name).cloned().or_else(|| {
                    self.options
                        .shapes
                        .iter()
                        .find(|sh| sh.name == name)
                        .map(|sh| sh.fields.clone())
                })?;

                Some(
                    declared
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                )
            },
            0,
        )
    }

    pub(crate) fn remote_decl(&mut self, r: &RemoteDecl) {
        let name = self.decl_name(r.name);
        let start = self.byte_start(r.span);
        let end = self.byte_end(r.span);

        if self.options.definitions {
            self.blank_lines(start, end);

            return;
        }

        for p in &r.params {
            // The wire layout reads each parameter by its name.
            if p.destructure.is_some() {
                let pattern = self.text_of(p.name).trim().to_string();
                self.diagnose(
                    p.name,
                    &format!("a remote packs no pattern: `{pattern}` has no name the wire layout can read; name the parameter"),
                );

                continue;
            }

            let Some(ty) = p.ty else { continue };
            let text = self.text_of(ty).to_string();

            // The layout packs what a type names, and `~T` names what a
            // value is not.
            if self.has_negation(ty) {
                let pname = self.text_of(p.name).to_string();
                let message = format!(
                    "a remote packs no negation: parameter `{pname}` has type `{text}`; name the types it carries"
                );
                self.diagnose(ty, &message);

                continue;
            }

            if let Some((field, bad, why)) = self.offender_of(&text) {
                let pname = self.text_of(p.name).to_string();
                let what = match field {
                    Some(f) => format!("parameter `{pname}` has field `{f}` of type `{bad}`"),

                    None => format!("parameter `{pname}` has type `{bad}`"),
                };
                self.diagnose(
                    ty,
                    &format!("remote `{name}`: {what}, which {why}; a remote carries only data"),
                );
            }
        }

        // The answer of a remote function crosses the wire too.
        if let Some(ty) = r.ret_type {
            let text = self.text_of(ty).to_string();

            if let Some((field, bad, why)) = self.offender_of(&text) {
                let what = match field {
                    Some(f) => format!("returns a value whose field `{f}` is `{bad}`"),

                    None => format!("returns `{bad}`"),
                };
                self.diagnose(
                    ty,
                    &format!("remote `{name}`: {what}, which {why}; a remote carries only data"),
                );
            }
        }

        let params: Vec<String> = r
            .params
            .iter()
            .map(|p| luau_string(self.text_of(p.name)))
            .collect();
        let defaults: Vec<String> = r
            .params
            .iter()
            .filter_map(|p| {
                p.default.as_ref().map(|d| {
                    let v = self.render_to_string(d);

                    format!("{} = {v}", self.text_of(p.name))
                })
            })
            .collect();
        let attrs = self.attr_table(&r.attributes);
        let wire = self.wire_layout(r);

        // The layout writes `struct = Shot`, the table the runtime sets as
        // the metatable of a decoded value. A struct declared below the
        // remote is a `local` the emit has not reached, so the name reads
        // as a nil global and the decoded value has no methods.
        for (p, w) in r.params.iter().zip(&wire) {
            let below = w
                .structs()
                .into_iter()
                .find(|s| self.struct_at.get(s).is_some_and(|at| *at > start));

            if let Some(s) = below {
                let pname = self.text_of(p.name).to_string();
                self.diagnose(
                    p.name,
                    &format!(
                        "parameter `{pname}` of remote `{name}` names struct `{s}`, which is declared below the remote; move the struct above it"
                    ),
                );
            }
        }

        // `@unreliable` rides an UnreliableRemoteEvent, and Roblox drops
        // a payload over 900 bytes. A parameter with no bound, a string
        // or an array, can pass it on any call.
        if r.attributes
            .iter()
            .any(|a| a.name.is_some_and(|n| self.text_of(n) == "unreliable"))
        {
            for (p, w) in r.params.iter().zip(&wire) {
                let Some(why) = w.unbounded() else { continue };
                let pname = self.text_of(p.name).to_string();
                self.diagnose(
                    p.name,
                    &format!(
                        "remote `{name}` is `@unreliable`, so its payload has to fit {UNRELIABLE_LIMIT} bytes; {why}, and parameter `{pname}` is one. Bound it, or drop `@unreliable`"
                    ),
                );

                break;
            }
        }

        let wire = if wire.iter().all(Wire::is_any) {
            String::new()
        } else {
            let kinds: Vec<String> = wire.iter().map(Wire::luau).collect();

            format!(", wire = {{ {} }}", kinds.join(", "))
        };
        let kind = if r.is_function { "function" } else { "event" };
        let std = self.std();
        let value = format!(
            "{std}.remote({{ name = {}, kind = \"{kind}\", from_client = {}, from_server = {}, params = {{ {} }}, defaults = {{ {} }}, attrs = {attrs}{wire} }})",
            luau_string(&name),
            r.from_client,
            r.from_server,
            params.join(", "),
            defaults.join(", ")
        );
        // The check artifact types the object by the declaration, so a
        // handler's parameters and a `fire` carry the declared types.
        let text = if self.options.check {
            let ty = self.remote_type(r);

            format!("local {name} = (({value} :: any) :: {ty})")
        } else {
            format!("local {name} = {value}")
        };
        self.generate(start, &text);
        self.blank_lines(start, end);

        if r.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// The wire layout of each parameter: a width attribute, `@u8`, on a
    /// number; else from the type, down into a struct, a table type, or
    /// an array of those; `any` for what crosses as it is. A `?` type or
    /// a default marks one that may be nil.
    pub(crate) fn wire_layout(&mut self, r: &RemoteDecl) -> Vec<Wire> {
        let mut kinds = Vec::new();
        let mut from = self.byte_end(r.name);

        for p in &r.params {
            let gap_at = from;
            let gap = self.src[from as usize..self.byte_start(p.name) as usize].to_string();
            from = self.byte_end(p.name);
            let width: Option<String> = gap.find('@').map(|at| {
                gap[at + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect()
            });
            // `@u8 @u16 x`: the first width packed and the second went
            // in silence. One report at each width after the first.
            let written = widths_in(&gap);

            for (at, w) in written.iter().skip(1) {
                let pname = self.text_of(p.name).to_string();
                let first = &written[0].1;
                let start = gap_at + at;
                self.diagnostics.push(Diagnostic {
                    start,
                    end: start + 1 + w.len() as u32,
                    message: format!("`{pname}` takes one wire width; `@{first}` is already on it"),
                });
            }
            let ty =
                p.ty.map(|t| self.text_of(t).trim().to_string())
                    .unwrap_or_else(|| "any".to_string());
            let width = width.filter(|w| WIRE_WIDTHS.contains(&w.as_str()));

            if let Some(w) = &width
                && let Some(base) = width_misfit(&ty)
            {
                let pname = self.text_of(p.name).to_string();
                self.diagnose(
                    p.name,
                    &format!("`@{w}` packs a `number`; parameter `{pname}` is `{base}`"),
                );
            }

            let wire = self
                .wire_of_type(&ty, width.as_deref(), 0)
                .with_optional(p.default.is_some());
            kinds.push(wire);
        }

        kinds
    }

    /// The layout of one type text. A struct declared here or in the
    /// project opens to its fields; a record type to its members; `T[]`,
    /// `{ T }`, and `Array<T>` to their item. Anything else is `any`.
    pub(crate) fn wire_of_type(&self, text: &str, width: Option<&str>, depth: usize) -> Wire {
        let mut ty = text.trim();
        let mut optional = false;

        while let Some(inner) = ty.strip_suffix('?') {
            ty = inner.trim();
            optional = true;
        }

        while ty.starts_with('(') && ty.ends_with(')') && group_len(ty, '(', ')') == Some(ty.len())
        {
            ty = ty[1..ty.len() - 1].trim();
        }

        let scalar = |kind: &str| Wire::Scalar {
            kind: kind.to_string(),
            optional,
        };

        if let Some(w) = width
            && (ty == "number" || ty == "any")
        {
            return scalar(w);
        }

        if depth > 6 {
            return scalar("any");
        }

        match ty {
            "number" => return scalar("f64"),
            "boolean" => return scalar("bool"),
            "string" => return scalar("str"),
            _ => {}
        }

        let array = |item: &str| Wire::Array {
            item: Box::new(self.wire_of_type(item, None, depth + 1)),
            optional,
        };

        if let Some(item) = ty.strip_suffix("[]") {
            return array(item);
        }

        for head in ["Array<", "ReadArray<", "WriteArray<"] {
            if let Some(rest) = ty.strip_prefix(head)
                && let Some(item) = rest.strip_suffix('>')
            {
                return array(item);
            }
        }

        if let Some(inner) = ty.strip_prefix('{')
            && let Some(inner) = inner.strip_suffix('}')
        {
            let inner = inner.trim();
            let parts = split_top_level(inner, ',');

            if parts.len() == 1 && !inner.contains(':') && !inner.starts_with('[') {
                return array(inner);
            }

            let mut fields = Vec::new();

            for part in parts {
                let part = part.trim();
                let part = part
                    .strip_prefix("read ")
                    .or_else(|| part.strip_prefix("write "))
                    .unwrap_or(part)
                    .trim();

                if part.is_empty() {
                    continue;
                }

                let Some(colon) = part.find(':') else {
                    return scalar("any");
                };
                let name = part[..colon].trim();

                if name.starts_with('[') || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return scalar("any");
                }

                fields.push((
                    name.to_string(),
                    self.wire_of_type(&part[colon + 1..], None, depth + 1),
                ));
            }

            if fields.is_empty() {
                return scalar("any");
            }

            return Wire::Table {
                fields,
                struct_name: None,
                optional,
            };
        }

        if ty.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let declared = self.struct_wire.get(ty).cloned().or_else(|| {
                self.options
                    .shapes
                    .iter()
                    .find(|sh| sh.name == ty)
                    .map(|sh| sh.fields.clone())
            });

            if let Some(declared) = declared {
                let fields = declared
                    .iter()
                    .map(|f| {
                        (
                            f.name.clone(),
                            self.wire_of_type(&f.ty, f.width.as_deref(), depth + 1),
                        )
                    })
                    .collect();

                return Wire::Table {
                    fields,
                    struct_name: Some(ty.to_string()),
                    optional,
                };
            }
        }

        scalar("any")
    }

    /// The type of a remote object, from its declaration. The side that
    /// fires passes the parameters, with a default making one optional;
    /// the side that handles gets them filled, and the server's handler
    /// gets the sender first. A remote open on both sides overloads.
    pub(crate) fn remote_type(&mut self, r: &RemoteDecl) -> String {
        let std = self.std();
        let mut fire_params = Vec::new();
        let mut handler_params = Vec::new();

        for p in &r.params {
            let name = self.text_of(p.name).to_string();
            // The surface is a type the checker reads, so the type
            // edits run over it: `T[]` is `Array<T>`, and a std name
            // takes the runtime's prefix.
            let ty =
                p.ty.map(|t| self.copy_type_to_string(t).trim().to_string())
                    .unwrap_or_else(|| "any".to_string());
            let optional = if p.default.is_some() && !ty.ends_with('?') {
                format!("{ty}?")
            } else {
                ty.clone()
            };
            fire_params.push(format!("{name}: {optional}"));
            handler_params.push(format!("{name}: {ty}"));
        }

        let fire = fire_params.join(", ");
        let handler = handler_params.join(", ");
        let with_player = |first: &str, rest: &str| {
            if rest.is_empty() {
                first.to_string()
            } else {
                format!("{first}, {rest}")
            }
        };
        let ret = r
            .ret_type
            .map(|t| self.copy_type_to_string(t).trim().to_string())
            .unwrap_or_else(|| "()".to_string());
        // A server handler of a remote function may answer with a Future.
        // `Future<()>` is no type, so an event's `call` yields any.
        let answer = if r.is_function && ret != "()" {
            format!("{ret} | {std}.Future<{ret}>")
        } else {
            ret.clone()
        };
        let future = if r.is_function && ret != "()" {
            format!("{std}.Future<{ret}>")
        } else {
            format!("{std}.Future<any>")
        };
        let connection = if r.is_function {
            "()"
        } else {
            "RBXScriptConnection"
        };

        // The client fires and the server handles.
        let client_fire = format!("({fire}) -> ()");
        let server_on = format!(
            "(handler: ({}) -> {answer}) -> {connection}",
            with_player("sender: Player", &handler)
        );
        let client_call = format!("({fire}) -> {future}");
        // The server fires and the client handles.
        let server_fire = format!("({}) -> ()", with_player("player: Player", &fire));
        let client_on = format!("(handler: ({handler}) -> {ret}) -> {connection}");
        let server_call = format!("({}) -> {future}", with_player("player: Player", &fire));

        let pick = |client: String, server: String| match (r.from_client, r.from_server) {
            (true, false) => client,
            (false, true) => server,
            _ => format!("({client}) & ({server})"),
        };
        // A `.client.aly` or `.server.aly` file sees one side of the
        // remote. Every other file is shared and sees both, as a module
        // that branches on `RunService` does.
        let side = self.file_side;
        let client_fires = r.from_client && side != Some(crate::directives::Side::Server);
        let server_fires = r.from_server && side != Some(crate::directives::Side::Client);
        let client_handles = r.from_server && side != Some(crate::directives::Side::Server);
        let server_handles = r.from_client && side != Some(crate::directives::Side::Client);
        // `instance` is the Roblox object the runtime makes, by kind, so
        // `Ping.instance:FireServer(1)` checks against the right class.
        let unreliable = r
            .attributes
            .iter()
            .any(|a| a.name.is_some_and(|n| self.text_of(n) == "unreliable"));
        let class = if r.is_function {
            "RemoteFunction"
        } else if unreliable {
            "UnreliableRemoteEvent"
        } else {
            "RemoteEvent"
        };
        // `calls` is the testing hook: outside Roblox the runtime records
        // each fire there instead of sending it, so a `@test` reads it.
        let mut members = vec![
            format!("spec: {std}.RemoteSpec"),
            format!("instance: {class}?"),
            format!("calls: {std}.RemoteCalls"),
        ];

        match (client_fires, server_fires) {
            (false, false) => {}

            (a, b) => {
                let ty = match (a, b) {
                    (true, false) => client_fire,
                    (false, true) => server_fire,
                    _ => pick(client_fire, server_fire),
                };
                let call = match (a, b) {
                    (true, false) => client_call,
                    (false, true) => server_call,
                    _ => pick(client_call, server_call),
                };
                members.push(format!("fire: {ty}"));

                // `call` asks and waits for an answer, so only a
                // `remote function` carries it.
                if r.is_function {
                    members.push(format!("call: {call}"));
                }
            }
        }

        // Only the server reaches every client.
        if server_fires {
            members.push(format!("fire_all: ({fire}) -> ()"));
            members.push(format!(
                "fire_except: ({}) -> ()",
                with_player("except: Player", &fire)
            ));
        }

        if client_handles || server_handles {
            let ty = match (client_handles, server_handles) {
                (true, false) => client_on,
                (false, true) => server_on,
                _ => pick(server_on, client_on),
            };
            members.push(format!("on: {ty}"));
            members.push(format!("once: {ty}"));
            // `wait` settles with the payload the handler would get, and
            // `await` yields the first of those values. With both sides
            // handling, the two payloads differ and the type stays open.
            let waited = match (client_handles, server_handles) {
                (true, false) => handler_params
                    .first()
                    .and_then(|p| p.split_once(": "))
                    .map(|(_, t)| t.to_string()),

                (false, true) => Some("Player".to_string()),

                _ => None,
            };
            let waited = waited.unwrap_or_else(|| "any".to_string());
            members.push(format!("wait: () -> {std}.Future<{waited}>"));
        }

        // The rate limit guards the client-to-server direction, so only
        // the server hears a sender it refused.
        if server_handles {
            members.push("on_ratelimited: (handler: (player: Player) -> ()) -> ()".to_string());
        }

        format!("{{ {} }}", members.join(", "))
    }

    // --- attributes ------------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use crate::EmitOptions;

    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// A remote's surface is a type the checker reads, so `T[]` lowers
    /// to `Array<T>` there and a std name takes the runtime's prefix.
    #[test]
    fn a_remote_surface_lowers_its_types() {
        let src = "remote function Names() -> string[] from client
remote function Many() -> Array<string> from client
remote Take(xs: string[]) from client
";
        let out = crate::compile(src).unwrap();
        assert!(!out.check.contains("string[]"), "{}", out.check);
        assert!(out.check.contains("__alloy.Array<string>"), "{}", out.check);
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    /// The check artifact carries the layout the ship artifact carries.
    /// It went missing there, and `RemoteSpec.wire` types it, so the two
    /// artifacts differed by a runtime field and not by a type.
    #[test]
    fn the_check_artifact_keeps_the_wire_layout() {
        let src = "remote Ping(n: number, tag: string) from client\n";
        let out = crate::compile_with(
            src,
            &crate::EmitOptions {
                check: true,
                ..crate::EmitOptions::default()
            },
        )
        .unwrap();
        let layout = "attrs = {}, wire = { \"f64\", \"str\" } })";
        assert!(out.ship.contains(layout), "{}", out.ship);
        assert!(out.check.contains(layout), "{}", out.check);
    }

    /// A parameter and a field pack at one width. Two widths merged in
    /// silence: the first packed and the second went.
    #[test]
    fn one_width_holds_a_parameter_and_a_field() {
        let src = "remote DoubleWidth(@u8 @u16 x: number) from client\n";
        assert_eq!(
            messages(src),
            vec!["`x` takes one wire width; `@u8` is already on it"]
        );

        let field = "struct Hit as\n    @u8\n    @u16\n    hp: number\nend\nremote SendHit(h: Hit) from client\n";
        assert_eq!(
            messages(field),
            vec!["`hp` takes one wire width; `@u8` is already on it"]
        );

        // One width is the form, and the wire keeps it.
        let one = "remote OneWidth(@u8 x: number) from client\n";
        assert!(messages(one).is_empty(), "{:?}", messages(one));
        assert!(
            crate::compile(one)
                .unwrap()
                .ship
                .contains("wire = { \"u8\" }"),
            "{}",
            crate::compile(one).unwrap().ship
        );
    }

    /// A width packs a number, on a field the way it does on a
    /// parameter. The wire spec read the field's own type and dropped the
    /// width, so an array field took one and crossed at 64 bits.
    #[test]
    fn a_width_on_an_array_field_reports() {
        let src = "struct Bag as\n    @u8\n    items: number[]\nend\nremote SyncBag(b: Bag) from client\n";
        assert_eq!(
            messages(src),
            vec!["`@u8` packs a `number`; field `items` is `number[]`"]
        );

        // The parameter form reads the same way.
        let param = "remote Bad(@u8 amounts: number[]) from client\n";
        assert_eq!(
            messages(param),
            vec!["`@u8` packs a `number`; parameter `amounts` is `number[]`"]
        );

        // A number field packs, and so does an optional one.
        let ok = "struct Ok as\n    @u8\n    hp: number\n    @u16\n    mp: number?\nend\nremote S(o: Ok) from client\n";
        assert!(messages(ok).is_empty(), "{:?}", messages(ok));
    }

    /// A `HashMap` keeps its methods on a metatable, which a remote
    /// strips. The answer of a remote function crosses the wire too.
    #[test]
    fn a_metatable_type_cannot_cross_a_remote() {
        let src = "remote Send(bag: HashMap<string, number>) from client
remote function Read() -> HashMap<string, number> from client
";
        let got = messages(src);
        assert!(
            got.iter().any(|m| m
                == "remote `Send`: parameter `bag` has type `HashMap<string, number>`, which carries a metatable that the wire cannot pack; a remote carries only data"),
            "{got:?}"
        );
        assert!(
            got.iter().any(|m| m
                == "remote `Read`: returns `HashMap<string, number>`, which carries a metatable that the wire cannot pack; a remote carries only data"),
            "{got:?}"
        );
        // An array still crosses.
        assert!(
            messages(
                "remote function Names() -> string[] from client
"
            )
            .is_empty()
        );
    }

    /// A struct packs down to any depth, so the wire check reads the
    /// fields of a struct a parameter names, not the name alone.
    #[test]
    fn a_struct_field_that_cannot_cross_a_remote_reports() {
        let src = "struct HasMethod as
    go: () -> ()
end

struct Wrapper as
    inner: HasMethod
end

remote TableWithFn(payload: HasMethod) from client
remote Nested(payload: Wrapper) from client
";
        let got = messages(src);
        assert!(
            got.iter().any(|m| m
                == "remote `TableWithFn`: parameter `payload` has field `go` of type `() -> ()`, which is a function type; a remote carries only data"),
            "{got:?}"
        );
        assert!(
            got.iter().any(|m| m
                == "remote `Nested`: parameter `payload` has field `inner.go` of type `() -> ()`, which is a function type; a remote carries only data"),
            "{got:?}"
        );
        // A struct of data still crosses, and a struct that holds itself
        // does not walk forever.
        assert!(
            messages(
                "struct Point as
    x: number
end

struct Node as
    value: number
    next: Node?
end

remote Move(p: Point) from client
remote Chain(n: Node) from client
"
            )
            .is_empty()
        );
    }

    /// `call` asks and waits for an answer, so a plain remote does not
    /// carry it. The surface named it anyway.
    #[test]
    fn only_a_remote_function_carries_call() {
        let plain = crate::compile(
            "export remote Chat(text: string) from client
",
        )
        .unwrap();
        assert!(!plain.check.contains("call:"), "{}", plain.check);
        assert!(plain.check.contains("fire:"), "{}", plain.check);
        let asked = crate::compile(
            "export remote function Ask(): number from client
",
        )
        .unwrap();
        assert!(asked.check.contains("call:"), "{}", asked.check);
    }

    /// The surface types what the editor reads: `wait` settles with the
    /// payload the handler would get, and `spec` is the declaration, not
    /// `any`.
    #[test]
    fn a_remote_surface_types_wait_and_spec() {
        let out = crate::compile("export remote Toast(text: string) from server\n").unwrap();
        assert!(
            out.check.contains("wait: () -> __alloy.Future<string>"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("spec: __alloy.RemoteSpec"),
            "{}",
            out.check
        );
        // The server handles a client's fire, and the sender comes first.
        let up = crate::compile("export remote Buy(offer_id: number) from client\n").unwrap();
        assert!(
            up.check.contains("wait: () -> __alloy.Future<Player>"),
            "{}",
            up.check
        );
    }

    /// `@unreliable` rides an UnreliableRemoteEvent, and Roblox drops a
    /// payload over 900 bytes.
    #[test]
    fn an_unreliable_remote_needs_a_bounded_payload() {
        let got = messages("@unreliable\nremote Bulk(chunk: string) from client\n");
        assert!(
            got.iter().any(|m| m
                == "remote `Bulk` is `@unreliable`, so its payload has to fit 900 bytes; a string has no length bound, and parameter `chunk` is one. Bound it, or drop `@unreliable`"),
            "{got:?}"
        );
        // A fixed-width payload passes, and a reliable remote is free.
        assert!(
            messages("@unreliable\nremote Tick(@u8 n: number, at: number) from server\n")
                .is_empty()
        );
        assert!(messages("@unreliable\nremote Move(cframe: CFrame) from client\n").is_empty());
        assert!(messages("remote Bulk(chunk: string) from client\n").is_empty());
    }

    #[test]
    fn a_wire_message_names_the_field_that_holds_the_function() {
        let src = "remote Deep(payload: { name: string, cb: (number) -> () }) from client\n";
        assert!(
            messages(src).iter().any(|m| m
                == "remote `Deep`: parameter `payload` has field `cb` of type `(number) -> ()`, which is a function type; a remote carries only data"),
            "{:?}",
            messages(src)
        );
    }

    /// `calls` is the testing hook the runtime fills outside Roblox.
    /// The check artifact declares it, so `#Damage.calls` in a `@test`
    /// type-checks on either side.
    #[test]
    fn a_remote_declares_its_calls_hook() {
        let src = "remote Damage(target: string, amount: number) from client\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.check.contains("calls: __alloy.RemoteCalls"),
            "{}",
            out.check
        );
    }

    /// `instance` is the Roblox object behind the remote, by kind, so a
    /// `:FireServer` on a remote function reports.
    #[test]
    fn a_remote_types_its_instance_by_kind() {
        let event = crate::compile("remote Ping(n: number) from client\n").unwrap();
        assert!(
            event.check.contains("instance: RemoteEvent?"),
            "{}",
            event.check
        );

        let unreliable =
            crate::compile("@unreliable\nremote Tick(@u8 n: number) from server\n").unwrap();
        assert!(
            unreliable
                .check
                .contains("instance: UnreliableRemoteEvent?"),
            "{}",
            unreliable.check
        );

        let function =
            crate::compile("remote function Ask(n: number) -> number from client\n").unwrap();
        assert!(
            function.check.contains("instance: RemoteFunction?"),
            "{}",
            function.check
        );
    }

    #[test]
    fn a_remote_shows_one_side_in_a_side_named_file() {
        let src = "export remote Damage(target: Player, amount: number) from client\n";
        let client = EmitOptions {
            check: true,
            file_name: "src/ui.client.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &client).unwrap();
        assert!(out.check.contains("fire: "), "{}", out.check);
        assert!(!out.check.contains("on: "), "{}", out.check);

        let server = EmitOptions {
            check: true,
            file_name: "src/main.server.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &server).unwrap();
        assert!(out.check.contains("on: "), "{}", out.check);
        assert!(!out.check.contains("fire: "), "{}", out.check);

        // A shared module branches on `RunService` and sees both.
        let shared = EmitOptions {
            check: true,
            file_name: "src/remotes.aly".to_string(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &shared).unwrap();
        assert!(out.check.contains("fire: "), "{}", out.check);
        assert!(out.check.contains("on: "), "{}", out.check);
    }
}
