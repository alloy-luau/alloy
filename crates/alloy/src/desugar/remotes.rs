//! Remote declarations and their wire layout.

use alloy_syntax::ast::{Block, Expr, IndexKey, RemoteDecl, Stmt};

use super::types::{group_len, split_top_level};
use super::*;

/// The number widths a parameter or a field may carry.
pub const WIRE_WIDTHS: &[&str] = &["u8", "u16", "u32", "i8", "i16", "i32", "f32", "f64"];

/// The whole numbers an integer width holds; a float width takes any.
pub(crate) fn width_range(width: &str) -> Option<(f64, f64)> {
    Some(match width {
        "u8" => (0.0, 255.0),

        "u16" => (0.0, 65535.0),

        "u32" => (0.0, 4294967295.0),

        "i8" => (-128.0, 127.0),

        "i16" => (-32768.0, 32767.0),

        "i32" => (-2147483648.0, 2147483647.0),

        _ => return None,
    })
}

/// The type text a wire width cannot pack. A width packs a number, and
/// `any` crosses as one; a trailing `?` makes no difference. `None` for a
/// type the width fits.
pub(crate) fn width_misfit(ty: &str) -> Option<&str> {
    let base = ty.trim_end_matches('?').trim();

    (base != "number" && base != "any").then_some(base)
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
    /// An enum of this file or an import. The value crosses beside the
    /// buffer as Roblox copies a table, which drops the metatable; the
    /// reader checks the variant and restores it, and each payload value
    /// by its own layout.
    Enum {
        name: String,
        /// Each variant with the layout of each payload value; a unit
        /// variant is a string.
        variants: Vec<(String, Vec<Wire>)>,
        optional: bool,
    },
}

/// The bytes Roblox carries on an UnreliableRemoteEvent.
pub(crate) const UNRELIABLE_LIMIT: usize = 900;

impl Wire {
    /// The most bytes the value packs to, or why it has no bound. A
    /// number, a boolean, and an Instance reference have a fixed size; a
    /// string, an array, and a value that crosses as a table grow with
    /// what the caller passes.
    fn max_size(&self) -> Result<usize, &'static str> {
        let flag = |optional: bool| usize::from(optional);

        match self {
            Wire::Scalar { kind, optional } => match kind.as_str() {
                "str" => Err("a string has no length bound"),

                // A unit enum's longest name, after its one-byte length.
                k if k.starts_with("one:") => Ok(flag(*optional)
                    + 1
                    + k["one:".len()..]
                        .split(',')
                        .map(str::len)
                        .max()
                        .unwrap_or(0)),

                "any" => Err("a value that crosses as a table has no size bound"),

                k if k.starts_with("inst:") => Ok(flag(*optional)),

                // A Roblox datatype crosses in the engine's own encoding,
                // at about this many bytes.
                k if k.starts_with("val:") => Ok(flag(*optional)
                    + match &k["val:".len()..] {
                        "Vector2" | "UDim" | "Vector2int16" => 8,

                        "Vector3" | "Color3" | "Vector3int16" => 12,

                        "UDim2" | "Rect" => 16,

                        "CFrame" => 48,

                        _ => 16,
                    }),

                "u8" | "i8" | "bool" => Ok(1 + flag(*optional)),

                "u16" | "i16" => Ok(2 + flag(*optional)),

                "u32" | "i32" | "f32" => Ok(4 + flag(*optional)),

                _ => Ok(8 + flag(*optional)),
            },

            Wire::Table {
                fields, optional, ..
            } => fields
                .iter()
                .try_fold(flag(*optional), |sum, (_, w)| Ok(sum + w.max_size()?)),

            Wire::Array { .. } => Err("an array has no length bound"),

            Wire::Enum { .. } => Err("a value that crosses as a table has no size bound"),
        }
    }

    /// Whether the value crosses beside the buffer: a table the layout
    /// cannot open, or an Instance.
    fn is_any(&self) -> bool {
        match self {
            Wire::Scalar { kind, .. } => {
                kind == "any" || kind.starts_with("inst:") || kind.starts_with("val:")
            }

            Wire::Enum { .. } => true,

            _ => false,
        }
    }

    /// Whether a buffer carries the value in fewer bytes than Roblox's
    /// own encoding: a narrowed number, a record, whose keys the buffer
    /// drops, and an array of fixed-size items, whose type tags it
    /// drops. A lone `f64`, `bool`, or string gains nothing.
    fn gains(&self) -> bool {
        match self {
            Wire::Scalar { kind, .. } => matches!(
                kind.as_str(),
                "u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "f32"
            ),

            Wire::Table { fields, .. } => fields.iter().any(|(_, w)| !w.is_any()),

            Wire::Array { item, .. } => {
                !item.is_any()
                    && !matches!(item.as_ref(), Wire::Scalar { kind, .. } if kind == "str")
            }

            Wire::Enum { .. } => false,
        }
    }

    /// The path to a part a buffer cannot carry, a table the layout
    /// cannot open. An Instance crosses beside the buffer and counts as
    /// carried.
    fn opaque_path(&self, at: &str) -> Option<String> {
        match self {
            Wire::Scalar { kind, .. } if kind == "any" => Some(at.to_string()),

            Wire::Scalar { .. } | Wire::Enum { .. } => None,

            Wire::Table { fields, .. } => fields
                .iter()
                .find_map(|(n, w)| w.opaque_path(&format!("{at}.{n}"))),

            Wire::Array { item, .. } => item.opaque_path(&format!("{at}[]")),
        }
    }

    /// Every struct and enum name the layout writes, at any depth.
    fn structs(&self) -> Vec<String> {
        match self {
            Wire::Scalar { .. } => Vec::new(),

            Wire::Enum { name, variants, .. } => std::iter::once(name.clone())
                .chain(variants.iter().flat_map(|(_, p)| p).flat_map(Wire::structs))
                .collect(),

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

            Wire::Enum {
                name,
                variants,
                optional,
            } => {
                let tags: Vec<String> = variants
                    .iter()
                    .map(|(v, p)| format!("{v} = {}", p.len()))
                    .collect();
                // A payload the layout leaves open, `any`, needs no slot.
                let slots: Vec<String> = variants
                    .iter()
                    .filter(|(_, p)| p.iter().any(|w| w.luau() != "\"any\""))
                    .map(|(v, p)| {
                        let nodes: Vec<String> = p.iter().map(Wire::luau).collect();

                        format!("{v} = {{ {} }}", nodes.join(", "))
                    })
                    .collect();
                let slots = match slots.is_empty() {
                    true => String::new(),

                    false => format!(", slots = {{ {} }}", slots.join(", ")),
                };
                let optional = if *optional { ", optional = true" } else { "" };

                format!(
                    "{{ enum = {name}, tags = {{ {} }}{slots}{optional} }}",
                    tags.join(", ")
                )
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

/// The layout of an enum whose table `name` reads. A unit enum crosses
/// as one of its names; the reader refuses any other string, so a forged
/// `"Nuke"` reaches no handler.
fn enum_wire(name: String, variants: Vec<(String, Vec<Wire>)>, optional: bool) -> Wire {
    if variants.iter().all(|(_, p)| p.is_empty()) {
        let names: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).collect();

        return Wire::Scalar {
            kind: format!("one:{}", names.join(",")),
            optional,
        };
    }

    Wire::Enum {
        name,
        variants,
        optional,
    }
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
            "HashMap" | "Set" | "BitSet" | "Queue" | "Heap" | "Iter" => {
                return Some("carries a metatable that the wire cannot pack");
            }
            _ => {}
        }
    }

    None
}

impl<'s> Desugar<'s> {
    /// The module this file is, as the project shapes name it: the
    /// longest one its path ends with. A test build names the file
    /// `src/types.aly`, and the module is `types.aly`.
    pub(crate) fn own_module(&self) -> Option<&str> {
        let path = std::path::Path::new(&self.options.file_name);

        self.options
            .wire_scopes
            .iter()
            .map(|s| s.module.as_str())
            .filter(|m| !m.is_empty() && path.ends_with(m))
            .max_by_key(|m| m.len())
    }

    /*
    The project struct or enum that `ty` names in `module`: one the
    module declares, or one an import there binds, followed to the
    module that declares it. `K.Kind` reads through the star import `K`.

    A layout read "the one project type of this name". A private struct of
    the same name in an unrelated file then made the name ambiguous, and
    the layout lost its struct, its enum slots, or its registration.
    */
    pub(crate) fn project_type(
        &self,
        module: &str,
        ty: &str,
        hops: usize,
    ) -> Option<&crate::StructShape> {
        // Two modules that import a name from each other would loop.
        if hops > 8 {
            return None;
        }

        let scope = self.options.wire_scopes.iter().find(|s| s.module == module);

        if let Some((star, name)) = ty.split_once('.') {
            let (_, target) = scope?.stars.iter().find(|(local, _)| local == star)?;

            return self.project_type(target, name, hops + 1);
        }

        if let Some(shape) = self
            .options
            .shapes
            .iter()
            .find(|sh| sh.module == module && sh.name == ty)
        {
            return Some(shape);
        }

        let (_, target, name) = scope?.names.iter().find(|(local, ..)| local == ty)?;

        self.project_type(target, name, hops + 1)
    }

    /// The struct or enum another file declares, for a name an import
    /// of this file binds: `Item` by a named import, or `Ty.Item`
    /// through a star import. A struct of the same name that this file
    /// never imports is not the type the parameter names: a `Player`
    /// struct elsewhere made `target: Player` a table layout, and the
    /// remote then refused every real Player.
    pub(crate) fn imported_type(&self, ty: &str) -> Option<&crate::StructShape> {
        let bound = match ty.split_once('.') {
            Some((star, _)) => self.star_modules.contains(star),

            None => self.imported_names.contains(ty),
        };

        if !bound {
            return None;
        }

        self.project_type(self.own_module()?, ty, 0)
    }

    /// The layout of each variant of an enum, from the payload types. An
    /// enum of another file names its `module`, whose names they are.
    fn variant_wires(
        &self,
        variants: &[(String, Vec<String>)],
        module: Option<&str>,
        depth: usize,
    ) -> Vec<(String, Vec<Wire>)> {
        variants
            .iter()
            .map(|(v, types)| {
                let wires = types
                    .iter()
                    .map(|t| self.wire_of_type(t, None, depth + 1, module))
                    .collect();

                (v.clone(), wires)
            })
            .collect()
    }

    /// The layout of each field of a struct another file declares. The
    /// field types belong to that file.
    fn shape_fields(&self, shape: &crate::StructShape, depth: usize) -> Vec<(String, Wire)> {
        shape
            .fields
            .iter()
            .map(|f| {
                let w =
                    self.wire_of_type(&f.ty, f.width.as_deref(), depth + 1, Some(&shape.module));

                (f.name.clone(), w)
            })
            .collect()
    }

    /*
    The registration of a struct or an enum of this file, for the wire of
    another file. A remote there that carries a struct from here reads
    each field's type in this file, and a field can name a table that
    file cannot reach. Its layout names the key instead, and the reader
    takes the table from the registry. Only a type that a struct field of
    the project names registers, so the output of other types stays as
    it was.
    */
    pub(crate) fn wire_registration(&mut self, name: &str) -> String {
        if !self.at_top_level() || self.options.macro_depth > 0 {
            return String::new();
        }

        let Some(own) = self.own_module() else {
            return String::new();
        };
        let Some(shape) = self
            .options
            .shapes
            .iter()
            .find(|sh| sh.module == own && sh.name == name)
        else {
            return String::new();
        };
        // A field names this table when its module's imports lead to it.
        let in_a_field = self.options.shapes.iter().any(|sh| {
            sh.fields
                .iter()
                .map(|f| &f.ty)
                .chain(sh.variants.iter().flat_map(|(_, p)| p))
                .any(|ty| {
                    ty.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
                        .filter_map(|w| self.project_type(&sh.module, w, 0))
                        .any(|t| std::ptr::eq(t, shape))
                })
        });
        // A unit enum crosses as a string and needs no table.
        let unit = !shape.variants.is_empty() && shape.variants.iter().all(|(_, p)| p.is_empty());

        if !in_a_field || unit {
            return String::new();
        }

        let key = luau_string(&shape.wire_key());

        format!(" {}.wire.types[{key}] = {name}", self.std())
    }

    /// `wire_offender` with the structs of this file, so a parameter
    /// that names one reports the field that cannot cross the wire.
    fn offender_of(&self, ty: &str) -> Option<Offender> {
        offender(
            ty,
            &|name| {
                let declared = match self.struct_wire.get(name) {
                    Some(fields) => fields.clone(),

                    None => self
                        .imported_type(name)
                        .filter(|sh| sh.variants.is_empty())?
                        .fields
                        .clone(),
                };

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
                        "parameter `{pname}` of remote `{name}` names `{s}`, which is declared below the remote; move the declaration above it"
                    ),
                );
            }
        }

        // `@unreliable` rides an UnreliableRemoteEvent, and Roblox drops
        // a payload over 900 bytes. A parameter with no bound, a string
        // or an array, can pass it on any call, and fixed sizes add up.
        if r.attributes
            .iter()
            .any(|a| a.name.is_some_and(|n| self.text_of(n) == "unreliable"))
        {
            let mut total = 0;

            for (p, w) in r.params.iter().zip(&wire) {
                let pname = self.text_of(p.name).to_string();

                match w.max_size() {
                    Ok(size) => total += size,

                    Err(why) => {
                        self.diagnose(
                            p.name,
                            &format!(
                                "remote `{name}` is `@unreliable`, so its payload has to fit {UNRELIABLE_LIMIT} bytes; {why}, and parameter `{pname}` is one. Bound it, or drop `@unreliable`"
                            ),
                        );
                        total = 0;

                        break;
                    }
                }
            }

            if total > UNRELIABLE_LIMIT {
                self.diagnose(
                    r.name,
                    &format!(
                        "remote `{name}` is `@unreliable`, so its payload has to fit {UNRELIABLE_LIMIT} bytes; its parameters pack to {total}. Narrow them with `@u8`, `@u16`, or `@f32`, or drop `@unreliable`"
                    ),
                );
            }
        }

        // `@wire(buffer)` or `@wire(table)` says how the payload
        // travels. With neither, the buffer carries it when one of the
        // parameters packs smaller there; Roblox's own encoding carries
        // it otherwise, checked on arrival against the same layout.
        // One remote takes one wire mode; a second `@wire` would lose.
        let extra: Vec<TokSpan> = r
            .attributes
            .iter()
            .filter(|a| a.name.is_some_and(|n| self.text_of(n) == "wire"))
            .skip(1)
            .map(|a| a.span)
            .collect();

        for at in extra {
            self.diagnose(at, "a remote takes one `@wire`; keep `buffer` or `table`");
        }

        let chosen = r
            .attributes
            .iter()
            .find(|a| a.name.is_some_and(|n| self.text_of(n) == "wire"))
            .and_then(|a| {
                a.args
                    .first()
                    .map(|x| (a.span, self.text_of(x.span()).to_string()))
            });
        let packs = match chosen.as_ref().map(|(_, m)| m.as_str()) {
            Some("buffer") => {
                for (p, w) in r.params.iter().zip(&wire) {
                    let pname = self.text_of(p.name).to_string();

                    if let Some(path) = w.opaque_path(&pname)
                        && let Some((at, _)) = &chosen
                    {
                        self.diagnose(
                            *at,
                            &format!(
                                "remote `{name}` is `@wire(buffer)`, and `{path}` is a table the layout cannot open, a map or an untyped value, which a buffer cannot hold; type it, or drop `@wire(buffer)`"
                            ),
                        );

                        break;
                    }
                }

                true
            }

            Some(_) => false,

            None => wire.iter().any(Wire::gains),
        };

        // A value beside the buffer needs no layout, unless the reader
        // restores an enum's metatable on it.
        let wire = if wire
            .iter()
            .all(|w| w.is_any() && !matches!(w, Wire::Enum { .. }))
        {
            String::new()
        } else {
            let kinds: Vec<String> = wire.iter().map(Wire::luau).collect();
            let mode = if packs { "" } else { ", pack = false" };

            format!(", wire = {{ {} }}{mode}", kinds.join(", "))
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

    /// The remotes this file declares join the imported ones.
    pub(crate) fn note_remote_sides(&mut self, block: &Block) {
        for stmt in &block.stmts {
            if let Stmt::Remote(r) = stmt.under_default() {
                let name = self.text_of(r.name).to_string();
                self.remote_sides
                    .insert(name, (r.from_client, r.from_server));
            }
        }
    }

    /*
    `Up.fire(1)` in a `.server.aly` file, where `Up` goes from the client.
    A remote declared in a shared module types both sides, so the checker
    took the call, and the server called `FireClient(1)` at run time. The
    other way, `Down.fire(1)` on the client, the checker asked for a
    `Player` and did not name the side.

    A file with a side runs on that side alone, so the check is sound
    there. A shared file may run on either side and gets no report.
    */
    pub(crate) fn check_remote_side(&mut self, e: &Expr) {
        use crate::directives::Side;

        let Some(side) = self.file_side else {
            return;
        };
        let Expr::Call {
            func, method: None, ..
        } = e
        else {
            return;
        };
        let Expr::Index {
            object,
            key: IndexKey::Field(verb),
            ..
        } = func.as_ref()
        else {
            return;
        };
        let receiver = self.text_of(object.span()).trim().to_string();
        let Some(&(from_client, from_server)) = self.remote_sides.get(&receiver) else {
            return;
        };
        let verb = self.text_of(*verb).to_string();
        let (sends, receives) = match side {
            Side::Client => (from_client, from_server),

            Side::Server => (from_server, from_client),
        };
        let other = match side {
            Side::Client => "server",

            Side::Server => "client",
        };
        let here = side.name();
        let message = match verb.as_str() {
            "fire" | "call" | "fire_all" | "fire_except" if !sends => {
                let act = if verb == "call" { "call" } else { "fire" };

                format!("`{receiver}` goes from the {other}; the {here} cannot {act} it")
            }

            // Only the server reaches every client.
            "fire_all" | "fire_except" if side == Side::Client => {
                format!("`{receiver}.{verb}` reaches the clients; only the server calls it")
            }

            "on" | "once" | "wait" if !receives => {
                format!("`{receiver}` goes from the {here}; the {here} cannot handle it")
            }

            _ => return,
        };
        self.diagnose(func.span(), &message);
    }

    /// The wire layout of each parameter: a width attribute, `@u8`, on a
    /// number; else from the type, down into a struct, a table type, or
    /// an array of those; `any` for what crosses as it is. A `?` type or
    /// a default marks one that may be nil.
    pub(crate) fn wire_layout(&mut self, r: &RemoteDecl) -> Vec<Wire> {
        let mut kinds = Vec::new();

        for p in &r.params {
            // The widths the parameter's own attributes write. `@u8 @u16
            // x`: the first packs, and each one after it reports.
            let written: Vec<(TokSpan, String)> = p
                .attributes
                .iter()
                .filter_map(|a| {
                    let n = self.text_of(a.name?).to_string();

                    WIRE_WIDTHS.contains(&n.as_str()).then_some((a.span, n))
                })
                .collect();

            for (at, _) in written.iter().skip(1) {
                let pname = self.text_of(p.name).to_string();
                let first = &written[0].1;
                self.diagnose(
                    *at,
                    &format!("`{pname}` takes one wire width; `@{first}` is already on it"),
                );
            }

            let width = written.first().map(|(_, w)| w.clone());
            let ty =
                p.ty.map(|t| self.text_of(t).trim().to_string())
                    .unwrap_or_else(|| "any".to_string());

            // `type Slot = number` is a number to the width, as the layout
            // reads the alias through.
            let seen = self
                .alias_values
                .get(ty.trim_end_matches('?').trim())
                .cloned();

            if let Some(w) = &width
                && let Some(base) = width_misfit(seen.as_deref().unwrap_or(&ty))
            {
                let pname = self.text_of(p.name).to_string();
                self.diagnose(
                    p.name,
                    &format!("`@{w}` packs a `number`; parameter `{pname}` is `{base}`"),
                );
            }

            // A default fills before the pack, so one the width cannot
            // hold fails every fire that leaves the argument out.
            if let (Some(w), Some(d)) = (&width, &p.default)
                && let Some((lo, hi)) = width_range(w)
                && let Ok(v) = self
                    .text_of(d.span())
                    .replace(['_', ' '], "")
                    .parse::<f64>()
                && (v < lo || v > hi || v.fract() != 0.0)
            {
                let pname = self.text_of(p.name).to_string();
                let shown = self.text_of(d.span()).to_string();
                self.diagnose(
                    d.span(),
                    &format!("`@{w}` holds a whole number from {lo} to {hi}; the default of `{pname}`, {shown}, does not fit"),
                );
            }

            // A default fills before the pack and after the read, so the
            // value is never nil on the wire.
            let wire = self.wire_of_type(&ty, width.as_deref(), 0, None);
            kinds.push(wire);
        }

        kinds
    }

    /// The layout of one type text. A struct declared here or in the
    /// project opens to its fields; a record type to its members; `T[]`,
    /// `{ T }`, and `Array<T>` to their item. Anything else is `any`.
    /*
    The layout of a type as written. `foreign` names the module of a type
    written in another file, a field of an imported struct: its names
    belong to that file, so this file's enums, aliases, and structs do not
    answer for them. There a name is a project type that the module's
    imports reach, one of the two engine names a file never shadows, a
    datatype, or a value the layout leaves alone.
    */
    pub(crate) fn wire_of_type(
        &self,
        text: &str,
        width: Option<&str>,
        depth: usize,
        foreign: Option<&str>,
    ) -> Wire {
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
            item: Box::new(self.wire_of_type(item, None, depth + 1, foreign)),
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
                    self.wire_of_type(&part[colon + 1..], None, depth + 1, foreign),
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

        let is_name = ty.chars().all(|c| c.is_alphanumeric() || c == '_');
        let is_path = ty
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.');

        if let Some(module) = foreign
            && is_path
        {
            // The project struct or enum the module's imports reach. Its
            // table is not in scope here, so the layout names the key the
            // declaring file registers the table under.
            if let Some(shape) = self.project_type(module, ty, 0) {
                let key = luau_string(&shape.wire_key());

                if !shape.variants.is_empty() {
                    let variants = self.variant_wires(&shape.variants, Some(&shape.module), depth);

                    return enum_wire(key, variants, optional);
                }

                return Wire::Table {
                    fields: self.shape_fields(shape, depth),
                    struct_name: Some(key),
                    optional,
                };
            }

            return match ty {
                "Instance" | "Player" => scalar(&format!("inst:{ty}")),

                _ if crate::roblox_classes::DATATYPES.contains(&ty) => scalar(&format!("val:{ty}")),

                _ => scalar("any"),
            };
        }

        // A struct of this file reads its fields here. An imported one,
        // by its name or through a star import as `Ty.Stats`, is in
        // scope under what the file writes, and reads its fields in the
        // file that declares it.
        if is_name && let Some(declared) = self.struct_wire.get(ty) {
            let fields = declared
                .iter()
                .map(|f| {
                    let w = self.wire_of_type(&f.ty, f.width.as_deref(), depth + 1, None);

                    (f.name.clone(), w)
                })
                .collect();

            return Wire::Table {
                fields,
                struct_name: Some(ty.to_string()),
                optional,
            };
        }

        if let Some(shape) = self.imported_type(ty).filter(|sh| sh.variants.is_empty()) {
            return Wire::Table {
                fields: self.shape_fields(shape, depth),
                struct_name: Some(ty.to_string()),
                optional,
            };
        }

        // The file's own type wins over a Roblox class of the same name,
        // as a struct does above: an `enum Team` sent as `inst:Team` made
        // the reader refuse every packet. An alias reads as its value, a
        // unit enum crosses as its string, and any other declaration, or
        // an import, as a value the layout leaves to the checker. An
        // enum through a star import reads under its path, `Ty.Item`.
        if is_path {
            if let Some(value) = self.alias_values.get(ty).cloned() {
                let value = if optional {
                    format!("({value})?")
                } else {
                    value
                };

                return self.wire_of_type(&value, width, depth + 1, None);
            }

            // An enum of this file reads its payload types here; an
            // imported one reads them in the file that declares it. With
            // neither, a payload value crosses as it is.
            if let Some(variants) = self.enum_decls.get(ty) {
                let variants = match self.enum_payloads.get(ty) {
                    Some(payloads) => self.variant_wires(payloads, None, depth),

                    None => match self.imported_type(ty).filter(|sh| !sh.variants.is_empty()) {
                        Some(shape) => {
                            self.variant_wires(&shape.variants, Some(&shape.module), depth)
                        }

                        None => variants
                            .iter()
                            .map(|(v, n)| (v.clone(), vec![scalar("any"); *n]))
                            .collect(),
                    },
                };

                return enum_wire(ty.to_string(), variants, optional);
            }

            if self.declared_types.contains(ty) || self.imported_names.contains(ty) {
                return scalar("any");
            }
        }

        // An Instance crosses as the engine's reference, beside the
        // buffer; the reader checks its class. A Roblox datatype crosses
        // beside it too, in the engine's own encoding, and the reader
        // checks its `typeof`. A struct of the same name, `Stats`, won
        // above.
        if ty == "Instance"
            || ty == "Player"
            || crate::roblox_classes::INSTANCE_CLASSES.contains(&ty)
        {
            return scalar(&format!("inst:{ty}"));
        }

        if crate::roblox_classes::DATATYPES.contains(&ty) {
            return scalar(&format!("val:{ty}"));
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

    /// The imports of `module`: each name with the module and the name
    /// there, and each star import with its module.
    fn scope(
        module: &str,
        names: &[(&str, &str, &str)],
        stars: &[(&str, &str)],
    ) -> crate::WireScope {
        crate::WireScope {
            module: module.into(),
            names: names
                .iter()
                .map(|(l, m, n)| (l.to_string(), m.to_string(), n.to_string()))
                .collect(),
            stars: stars
                .iter()
                .map(|(l, m)| (l.to_string(), m.to_string()))
                .collect(),
        }
    }

    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// A payload enum crosses as a table, and Roblox drops its
    /// metatable. The layout names the enum and its variants wherever
    /// it sits, so the reader restores it; an enum declared below the
    /// remote is nil when the layout is built, so it reports.
    #[test]
    fn a_payload_enum_names_itself_in_the_layout() {
        let src = "enum Boost\n    None\n    Strength(number)\nend\nstruct Reward\n    boost: Boost\nend\nremote Grant(boost: Boost, maybe: Boost?) from server\nremote Give(reward: Reward, all: Boost[]) from server\n";
        let out = crate::compile(src).unwrap();
        let node = "{ enum = Boost, tags = { None = 0, Strength = 1 }, slots = { Strength = { \"f64\" } } }";

        assert!(messages(src).is_empty(), "{:?}", messages(src));
        assert!(
            out.ship.contains(&format!(
                "wire = {{ {node}, {{ enum = Boost, tags = {{ None = 0, Strength = 1 }}, slots = {{ Strength = {{ \"f64\" }} }}, optional = true }} }}, pack = false"
            )),
            "{}",
            out.ship
        );
        assert!(
            out.ship.contains(&format!("{{ \"boost\", {node} }}")),
            "{}",
            out.ship
        );
        assert!(
            out.ship
                .contains(&format!("{{ item = {node}, array = true }}")),
            "{}",
            out.ship
        );

        let below = "remote Grant(boost: Boost) from server\nenum Boost\n    None\n    Strength(number)\nend\n";
        assert_eq!(
            messages(below),
            [
                "parameter `boost` of remote `Grant` names `Boost`, which is declared below the remote; move the declaration above it"
            ]
        );
    }

    /// A type from another module keeps its metatable across the wire. A
    /// star path is in scope, so the layout names it. A field of an
    /// imported struct can name a table this file cannot reach, so the
    /// layout names a key, and the declaring file registers the table
    /// under it. The layouts had no enum at all, `"any"`, and a record
    /// with no struct.
    #[test]
    fn a_type_from_another_module_keeps_its_metatable() {
        let field = |name: &str, ty: &str| crate::WireField {
            name: name.into(),
            ty: ty.into(),
            width: None,
        };
        let shape = |name: &str, fields, variants| crate::StructShape {
            name: name.into(),
            fields,
            module: "types.aly".into(),
            variants,
            ..Default::default()
        };
        let variants = vec![
            ("Sword".to_string(), vec!["number".to_string()]),
            ("Nothing".to_string(), Vec::new()),
        ];
        let options = EmitOptions {
            shapes: vec![
                shape("Item", Vec::new(), variants),
                shape("Stats", vec![field("hp", "number")], Vec::new()),
                shape("Outer", vec![field("s", "Stats")], Vec::new()),
                shape("Bag", vec![field("one", "Item")], Vec::new()),
            ],
            wire_scopes: vec![
                scope("types.aly", &[], &[]),
                scope(
                    "net.aly",
                    &[("Bag", "types.aly", "Bag"), ("Outer", "types.aly", "Outer")],
                    &[("Ty", "types.aly")],
                ),
            ],
            file_name: "net.aly".into(),
            import_enums: vec![(
                "Ty.Item".to_string(),
                vec![("Sword".to_string(), 1), ("Nothing".to_string(), 0)],
            )],
            ..EmitOptions::default()
        };
        let src = "import { Bag, Outer } from \"./types\"\nimport * as Ty from \"./types\"\nremote R3(bag: Bag) from server\nremote R5(it: Ty.Item) from server\nremote R6(o: Outer) from server\nremote R7(s: Ty.Stats) from server\n";
        let out = crate::compile_with(src, &options).unwrap();
        let item = "tags = { Sword = 1, Nothing = 0 }, slots = { Sword = { \"f64\" } } }";

        for layout in [
            format!("{{ fields = {{ {{ \"one\", {{ enum = \"types.aly:Item\", {item} }} }}, struct = Bag }}"),
            format!("wire = {{ {{ enum = Ty.Item, {item} }}"),
            "{ fields = { { \"s\", { fields = { { \"hp\", \"f64\" } }, struct = \"types.aly:Stats\" } } }, struct = Outer }".to_string(),
            "{ fields = { { \"hp\", \"f64\" } }, struct = Ty.Stats }".to_string(),
        ] {
            assert!(out.ship.contains(&layout), "{layout}\n{}", out.ship);
        }

        // The declaring file registers what a field names, on the line
        // of its `end`, and nothing else.
        let types = "export enum Item\n    Sword(number)\n    Nothing\nend\nexport struct Stats\n    hp: number\nend\nexport struct Outer\n    s: Stats\nend\nexport struct Bag\n    one: Item\nend\n";
        let declaring = EmitOptions {
            file_name: "types.aly".into(),
            ..options
        };
        let out = crate::compile_with(types, &declaring).unwrap();
        assert_eq!(
            out.ship.lines().count(),
            types.lines().count(),
            "{}",
            out.ship
        );

        for (line, name) in [(3, "Item"), (6, "Stats")] {
            let registers = format!("__alloy.wire.types[\"types.aly:{name}\"] = {name}");
            assert!(
                out.ship.lines().nth(line).unwrap().contains(&registers),
                "{}",
                out.ship
            );
        }

        assert_eq!(out.ship.matches("wire.types").count(), 2, "{}", out.ship);
    }

    /// A layout reads a type name through the imports of the file that
    /// writes it. A private `Inner` in an unrelated module made the name
    /// ambiguous: the enum slot went, the array item read `any`, and the
    /// declaring file stopped registering. A unit enum field of an
    /// imported struct crossed as a plain string.
    #[test]
    fn a_layout_reads_a_type_through_the_imports_of_its_file() {
        let field = |name: &str, ty: &str| crate::WireField {
            name: name.into(),
            ty: ty.into(),
            width: None,
        };
        let shape = |module: &str, name: &str, fields, variants| crate::StructShape {
            name: name.into(),
            fields,
            module: module.into(),
            variants,
            ..Default::default()
        };
        let kind = "shared/kind.aly";
        let options = EmitOptions {
            shapes: vec![
                shape(
                    "other.aly",
                    "Inner",
                    vec![field("label", "string")],
                    Vec::new(),
                ),
                shape(
                    "shared/inner.aly",
                    "Inner",
                    vec![field("n", "number")],
                    Vec::new(),
                ),
                shape(
                    kind,
                    "Kind",
                    Vec::new(),
                    vec![
                        ("Big".into(), vec!["Inner".into()]),
                        ("Small".into(), Vec::new()),
                    ],
                ),
                shape(
                    kind,
                    "Rarity",
                    Vec::new(),
                    vec![("Common".into(), Vec::new()), ("Rare".into(), Vec::new())],
                ),
                shape(
                    kind,
                    "Holder",
                    vec![field("list", "{ Inner }"), field("rarity", "Rarity?")],
                    Vec::new(),
                ),
            ],
            wire_scopes: vec![
                scope("other.aly", &[], &[]),
                scope("shared/inner.aly", &[], &[]),
                scope(kind, &[("Inner", "shared/inner.aly", "Inner")], &[]),
                scope("net.aly", &[("Holder", kind, "Holder")], &[("K", kind)]),
            ],
            file_name: "net.aly".into(),
            import_enums: vec![(
                "K.Kind".into(),
                vec![("Big".into(), 1), ("Small".into(), 0)],
            )],
            ..EmitOptions::default()
        };
        let src = "import * as K from \"./shared/kind\"\nimport { Holder } from \"./shared/kind\"\nremote R1(k: K.Kind) from client\nremote R2(h: Holder) from client\n";
        let out = crate::compile_with(src, &options).unwrap();
        let inner = "{ fields = { { \"n\", \"f64\" } }, struct = \"shared/inner.aly:Inner\" }";

        for layout in [
            format!(
                "{{ enum = K.Kind, tags = {{ Big = 1, Small = 0 }}, slots = {{ Big = {{ {inner} }} }} }}"
            ),
            format!(
                "{{ fields = {{ {{ \"list\", {{ item = {inner}, array = true }} }}, {{ \"rarity\", \"one:Common,Rare?\" }} }}, struct = Holder }}"
            ),
        ] {
            assert!(out.ship.contains(&layout), "{layout}\n{}", out.ship);
        }

        // The declaring file registers its table, and the private one
        // registers nothing.
        let registers = |file: &str, src: &str| {
            let options = EmitOptions {
                file_name: file.into(),
                ..options.clone()
            };

            crate::compile_with(src, &options)
                .unwrap()
                .ship
                .contains("wire.types")
        };
        assert!(registers(
            "src/shared/inner.aly",
            "export struct Inner\n    n: number\nend\n"
        ));
        assert!(!registers(
            "src/other.aly",
            "struct Inner\n    label: string\nend\n"
        ));
    }

    /// A struct inside a variant's payload keeps its metatable: the
    /// layout gives each payload value its own node, direct for a table
    /// in scope and keyed for one the declaring file registers. The
    /// payload crossed as it was, a plain table.
    #[test]
    fn a_payload_value_takes_its_own_layout() {
        let src = "struct Stats
    hp: number
end
enum Purchase
    Bought(Stats, number)
    Denied(string)
    Pending
end
remote R(p: Purchase) from server
";
        let out = crate::compile(src).unwrap();
        assert!(messages(src).is_empty(), "{:?}", messages(src));
        assert!(
            out.ship.contains("{ enum = Purchase, tags = { Bought = 2, Denied = 1, Pending = 0 }, slots = { Bought = { { fields = { { \"hp\", \"f64\" } }, struct = Stats }, \"f64\" }, Denied = { \"str\" } } }"),
            "{}",
            out.ship
        );

        // A struct declared below the remote is nil in the payload's
        // layout too.
        let below = "enum Purchase
    Bought(Stats)
end
remote R(p: Purchase) from server
struct Stats
    hp: number
end
";
        assert_eq!(
            messages(below),
            [
                "parameter `p` of remote `R` names `Stats`, which is declared below the remote; move the declaration above it"
            ]
        );

        // An imported enum reads its payload types in its own file, so a
        // struct there takes its key, and the file registers it.
        let options = EmitOptions {
            shapes: vec![
                crate::StructShape {
                    name: "Purchase".into(),
                    module: "shop.aly".into(),
                    variants: vec![("Bought".into(), vec!["Stats".into()])],
                    ..Default::default()
                },
                crate::StructShape {
                    name: "Stats".into(),
                    module: "shop.aly".into(),
                    fields: vec![crate::WireField {
                        name: "hp".into(),
                        ty: "number".into(),
                        width: None,
                    }],
                    ..Default::default()
                },
            ],
            wire_scopes: vec![
                scope("shop.aly", &[], &[]),
                scope("net.aly", &[("Purchase", "shop.aly", "Purchase")], &[]),
            ],
            file_name: "net.aly".into(),
            import_enums: vec![("Purchase".into(), vec![("Bought".into(), 1)])],
            ..EmitOptions::default()
        };
        let src = "import { Purchase } from \"./shop\"\nremote R(p: Purchase) from server\n";
        let out = crate::compile_with(src, &options).unwrap();
        assert!(
            out.ship.contains("{ enum = Purchase, tags = { Bought = 1 }, slots = { Bought = { { fields = { { \"hp\", \"f64\" } }, struct = \"shop.aly:Stats\" } } } }"),
            "{}",
            out.ship
        );

        let shop = "export struct Stats
    hp: number
end
export enum Purchase
    Bought(Stats)
end
";
        let declaring = EmitOptions {
            file_name: "shop.aly".into(),
            ..options
        };
        let out = crate::compile_with(shop, &declaring).unwrap();
        assert!(
            out.ship
                .contains("__alloy.wire.types[\"shop.aly:Stats\"] = Stats"),
            "{}",
            out.ship
        );
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
        // A lone number and string gain nothing in a buffer, so they
        // cross as Roblox encodes them, checked on arrival.
        let layout = "attrs = {}, wire = { \"f64\", \"str\" }, pack = false })";
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

    fn ship(src: &str) -> String {
        crate::compile(src).unwrap().ship
    }

    /// A width reads the parameter's own attributes: a `@u8` inside a
    /// default's string or a comment is no width.
    #[test]
    fn a_width_reads_only_its_own_attributes() {
        let out = ship("remote A(tag: string = \"@u8\", n: number) from client\n");
        assert!(out.contains("wire = { \"str\", \"f64\" }"), "{out}");
        let out = ship("remote B(x: number, -- keep @i8 small\n    y: number) from client\n");
        assert!(out.contains("wire = { \"f64\", \"f64\" }"), "{out}");
    }

    /// The buffer carries a payload when a parameter packs smaller
    /// there; Roblox's encoding carries the rest, and `@wire` decides.
    #[test]
    fn the_wire_mode_follows_the_gain_and_the_attribute() {
        assert!(ship("remote Chat(msg: string) from client\n").contains("pack = false"));
        assert!(!ship("remote Hp(@u8 hp: number) from server\n").contains("pack = false"));
        assert!(!ship("remote Pts(xs: number[]) from server\n").contains("pack = false"));
        assert!(
            !ship("@wire(buffer)\nremote Chat(msg: string) from client\n").contains("pack = false")
        );
        assert!(
            ship("@wire(table)\nremote Hp(@u8 hp: number) from server\n").contains("pack = false")
        );

        let got = messages("@wire(buffer)\nremote Sync(m: { [string]: number }) from server\n");
        assert!(
            got.iter().any(|m| m.starts_with(
                "remote `Sync` is `@wire(buffer)`, and `m` is a table the layout cannot open"
            )),
            "{got:?}"
        );
        let got = messages("@wire(json)\nremote Sync(n: number) from server\n");
        assert!(
            got.iter()
                .any(|m| m.starts_with("`@wire` takes `buffer` or `table`")),
            "{got:?}"
        );
    }

    /// A `@skip` field stays off the wire; a struct the file declares wins
    /// over a Roblox class of its name, and an Instance carries its class.
    #[test]
    fn the_layout_skips_and_names_what_it_should() {
        let out = ship(
            "struct Stats as\n    hp: number\n    @skip\n    cache: () -> ()\nend\nremote Sync(s: Stats, who: Player, at: Vector3) from server\n",
        );
        assert!(
            out.contains("{ fields = { { \"hp\", \"f64\" } }, struct = Stats }"),
            "{out}"
        );
        assert!(out.contains("\"inst:Player\", \"val:Vector3\""), "{out}");
    }

    /// Fixed sizes add up against the 900 bytes an unreliable remote
    /// carries.
    #[test]
    fn an_unreliable_payload_adds_its_fixed_sizes() {
        let fields: Vec<String> = (0..120).map(|i| format!("    f{i}: number")).collect();
        let src = format!(
            "struct Big as\n{}\nend\n@unreliable\nremote Blast(b: Big) from server\n",
            fields.join("\n")
        );
        let got = messages(&src);
        assert!(
            got.iter().any(|m| m.contains("its parameters pack to 960")),
            "{got:?}"
        );
    }
}
