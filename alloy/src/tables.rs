//! The plain tables of a source: `local X = { }` with members written
//! on it afterwards.
//!
//! Luau gives `self` no type in `function X:m()` on such a table, and
//! the analyzer has no name to print for the table either. The check
//! artifact writes the parameter out as `typeof(X)`, and the hover
//! folds read the printed shape back to the same words.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{CallArgs, Expr, IndexKey, Stmt, TableField};

/// The top-level `local X = { }` tables of a source with the members
/// each one carries: the keys of the literal, the `X.k = ...` writes,
/// and the functions written on it. A table the file rebinds or gives
/// a metatable is left out; a class of the `C.__index = C` shape holds
/// instances, and `typeof(C)` is the wrong type for their `self`.
pub fn plain_tables(src: &str) -> Vec<(String, Vec<String>)> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| -> String {
        let a = toks[span.start as usize].start as usize;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end as usize;

        src[a..b].to_string()
    };
    let mut order: Vec<String> = Vec::new();
    let mut members: HashMap<String, Vec<String>> = HashMap::new();
    let mut dropped: HashSet<String> = HashSet::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Local(l) = stmt.under_default() else {
            continue;
        };

        if l.names.len() != 1 || l.values.len() != 1 || l.names[0].destructure.is_some() {
            continue;
        }
        let Expr::Table { fields, .. } = &l.values[0] else {
            continue;
        };
        let name = text(l.names[0].name);

        if members.contains_key(&name) {
            dropped.insert(name);

            continue;
        }
        let keys = fields
            .iter()
            .filter_map(|f| match f {
                TableField::Named { name, .. } => Some(text(*name)),

                _ => None,
            })
            .collect();
        order.push(name.clone());
        members.insert(name, keys);
    }

    if members.is_empty() {
        return Vec::new();
    }

    for stmt in &parsed.chunk.block.stmts {
        match stmt.under_default() {
            Stmt::Assign(a) => {
                for target in &a.targets {
                    match target {
                        Expr::Name(n) => {
                            dropped.insert(text(*n));
                        }

                        Expr::Index {
                            object,
                            key: IndexKey::Field(k),
                            ..
                        } => {
                            let Expr::Name(n) = object.as_ref() else {
                                continue;
                            };
                            let owner = text(*n);
                            let key = text(*k);

                            // `C.__index = C` is a class, and its `self`
                            // is an instance, not the table.
                            if key.starts_with("__") {
                                dropped.insert(owner);
                            } else if let Some(list) = members.get_mut(&owner) {
                                list.push(key);
                            }
                        }

                        _ => {}
                    }
                }
            }

            Stmt::Function(f) if f.path.len() == 2 => {
                let owner = text(f.path[0]);

                if let Some(list) = members.get_mut(&owner) {
                    list.push(text(f.path[1]));
                }
            }

            Stmt::Call(Expr::Call { func, args, .. }, _) => {
                if let Expr::Name(name) = func.as_ref()
                    && text(*name) == "setmetatable"
                    && let CallArgs::Paren(list) = args
                    && let Some(Expr::Name(n)) = list.first()
                {
                    dropped.insert(text(*n));
                }
            }

            _ => {}
        }
    }

    order
        .into_iter()
        .filter(|name| !dropped.contains(name))
        .filter_map(|name| {
            let mut keys = members.remove(&name)?;

            if keys.is_empty() {
                return None;
            }

            keys.sort();
            keys.dedup();

            Some((name, keys))
        })
        .collect()
}
