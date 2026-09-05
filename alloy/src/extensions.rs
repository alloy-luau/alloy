//! The extension methods a file declares on foreign types. The language
//! server injects them into the analyzer's definitions, so a call such
//! as `v:flat()` types, completes, and hovers like a built-in method.

use std::collections::HashSet;

use alloy_syntax::ast::{Stmt, TokSpan};

use crate::desugar::PRIMITIVES;
use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

/// One method or static declared in `impl Target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub target: String,
    pub name: String,
    /// No `self` parameter: a static such as `Vector3.origin()`.
    pub is_static: bool,
    /// The parameters after `self`, as `name: type` pairs joined by `, `.
    pub params: String,
    /// The return type, when the method declares one.
    pub ret: Option<String>,
}

/// True for a type that is not an Alloy struct: an Instance class, a
/// datatype, or a primitive.
pub fn is_foreign(name: &str) -> bool {
    INSTANCE_CLASSES.contains(&name)
        || DATATYPES.contains(&name)
        || PRIMITIVES.contains(&name)
        || name == "Instance"
}

/// True for a primitive such as `string`. A primitive has no class block
/// in the definitions, so its extensions go through a helper table.
pub fn is_primitive(name: &str) -> bool {
    PRIMITIVES.contains(&name)
}

/// Every extension the source declares. A struct or enum declared in the
/// file is never foreign, whatever its name.
pub fn collect(src: &str) -> Vec<Extension> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };

    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| -> &str {
        if span.end <= span.start {
            return "";
        }

        let start = toks[span.start as usize].start as usize;
        let end = toks[span.end as usize - 1].end as usize;

        &src[start..end]
    };

    let stmts = &parsed.chunk.block.stmts;
    let mut local: HashSet<&str> = HashSet::new();

    for stmt in stmts {
        match stmt {
            Stmt::Struct(d) => {
                local.insert(text(d.name));
            }

            Stmt::Enum(d) => {
                local.insert(text(d.name));
            }

            _ => {}
        }
    }

    let mut out = Vec::new();

    for stmt in stmts {
        let Stmt::Impl(i) = stmt else {
            continue;
        };

        let target = text(i.target);

        if local.contains(target) || !is_foreign(target) {
            continue;
        }

        for m in &i.methods {
            let Some(first) = m.path.first() else {
                continue;
            };

            let params = &m.body.params;
            let has_self = params.first().is_some_and(|p| text(p.name) == "self");
            let rest = params.iter().skip(usize::from(has_self));
            let mut list = Vec::new();

            for p in rest {
                let mut ty = p.ty.map(text).unwrap_or("any").trim().to_string();

                // A default makes the parameter optional, as the emit does.
                if p.default.is_some() && !ty.ends_with('?') {
                    ty.push('?');
                }

                if p.is_vararg {
                    list.push(format!("...: {ty}"));
                } else {
                    list.push(format!("{}: {ty}", text(p.name)));
                }
            }

            out.push(Extension {
                target: target.to_string(),
                name: text(*first).to_string(),
                is_static: !has_self,
                params: list.join(", "),
                ret: m
                    .body
                    .ret_type
                    .map(|t| text(t).trim().trim_start_matches(':').trim().to_string())
                    .filter(|t| !t.is_empty()),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_and_statics() {
        let src = "export impl Vector3\n    function flat(self): Vector3\n        return self\n    end\n    function origin(): Vector3\n        return Vector3.zero\n    end\n    function scale(self, by: number, extra = 1)\n    end\nend\nstruct Vec2 as\n    x: number\nend\nimpl Vec2\n    function m(self) end\nend\n";
        let exts = collect(src);
        assert_eq!(exts.len(), 3, "{exts:#?}");
        assert_eq!(exts[0].name, "flat");
        assert!(!exts[0].is_static);
        assert_eq!(exts[0].ret.as_deref(), Some("Vector3"));
        assert_eq!(exts[1].name, "origin");
        assert!(exts[1].is_static);
        assert_eq!(exts[2].params, "by: number, extra: any?");
        assert_eq!(exts[2].ret, None);
    }
}
