//! The plain tables of a source: `local X = { }` with members written
//! on it afterwards.
//!
//! Luau gives `self` no type in `function X:m()` on such a table, and
//! the analyzer has no name to print for the table either. The check
//! artifact writes the parameter out as `typeof(X)`, and the hover
//! folds read the printed shape back to the same words.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{CallArgs, Expr, IndexKey, Stmt, TableField};

/// The `self` a colon method takes on a top-level `local X = { }`
/// table. Luau gives that `self` no type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfType {
    /// The table itself: `typeof(X)`.
    Table,
    /// An instance of a class of the `X.__index = X` shape. With a
    /// `function X.new`, it is the value that function returns, and the
    /// number is its parameter count. With none, the file says nothing
    /// of an instance's own fields.
    Instance(Option<usize>),
    /// The value the last top-level `X = ...` wrote. `typeof(X)` reads
    /// every value the local held, so an alias taken right after that
    /// statement, at this byte, names the last one.
    Rebound(u32),
}

impl SelfType {
    /// The type text of the parameter, and the alias the rebound form
    /// declares.
    pub fn text(&self, name: &str) -> String {
        match self {
            Self::Table => format!("typeof({name})"),

            Self::Instance(Some(n)) => {
                format!("typeof({name}.new({}))", vec!["nil :: any"; *n].join(", "))
            }

            Self::Instance(None) => {
                format!("typeof(setmetatable({{}} :: {{ [any]: any }}, {name}))")
            }

            Self::Rebound(_) => format!("__self_{name}"),
        }
    }
}

/// The `self` type of each top-level `local X = { }` table that a colon
/// method, or an `impl` method that takes `self`, is written on. A table is left out when its value is not
/// known at the method: a rebind after a method, or a metatable other
/// than its own class shape.
pub fn self_types(src: &str) -> HashMap<String, SelfType> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return HashMap::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();
    let end_byte = |span: alloy_syntax::ast::TokSpan| toks[(span.end as usize).max(1) - 1].end;
    let mut tables: HashSet<String> = HashSet::new();
    let mut dropped: HashSet<String> = HashSet::new();
    let mut methods: HashSet<String> = HashSet::new();
    let mut classes: HashSet<String> = HashSet::new();
    let mut metas: HashSet<String> = HashSet::new();
    let mut rebinds: HashMap<String, u32> = HashMap::new();
    let mut news: HashMap<String, usize> = HashMap::new();

    for stmt in &parsed.chunk.block.stmts {
        match stmt.under_default() {
            Stmt::Local(l) => {
                if l.names.len() == 1
                    && l.values.len() == 1
                    && l.names[0].destructure.is_none()
                    && matches!(l.values[0], Expr::Table { .. })
                    && !tables.insert(text(l.names[0].name))
                {
                    dropped.insert(text(l.names[0].name));
                }
            }

            Stmt::Assign(a) => {
                for target in &a.targets {
                    match target {
                        Expr::Name(n) => {
                            let name = text(*n);

                            // A method written before the rebind sits on
                            // the old table.
                            if methods.contains(&name) {
                                dropped.insert(name);
                            } else {
                                rebinds.insert(name, end_byte(stmt.span()));
                            }
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
                            let own = a.targets.len() == 1
                                && matches!(a.values.as_slice(), [Expr::Name(v)] if text(*v) == owner);

                            if key == "__index" && own {
                                classes.insert(owner);
                            } else if key.starts_with("__") {
                                metas.insert(owner);
                            }
                        }

                        _ => {}
                    }
                }
            }

            // `impl Ranged for X` writes methods on the table too.
            Stmt::Impl(i)
                if i.methods.iter().any(|m| {
                    m.body
                        .params
                        .first()
                        .is_some_and(|p| text(p.name) == "self")
                }) =>
            {
                methods.insert(text(i.target));
            }

            Stmt::Function(f) if f.path.len() == 2 => {
                let owner = text(f.path[0]);

                if f.is_method {
                    methods.insert(owner);
                } else if text(f.path[1]) == "new" {
                    let arity = f
                        .body
                        .params
                        .iter()
                        .filter(|p| text(p.name) != "...")
                        .count();
                    news.insert(owner, arity);
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

    tables
        .into_iter()
        .filter(|name| methods.contains(name) && !dropped.contains(name))
        .filter_map(|name| {
            let kind = match (classes.contains(&name), rebinds.get(&name)) {
                (true, Some(_)) => return None,

                (true, None) => SelfType::Instance(news.get(&name).copied()),

                (false, _) if metas.contains(&name) => return None,

                (false, Some(at)) => SelfType::Rebound(*at),

                (false, None) => SelfType::Table,
            };

            Some((name, kind))
        })
        .collect()
}

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
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A colon method on a table took no `self` type unless the table
    /// was plain. A class takes an instance, from its `new` when it has
    /// one, and a rebound table takes an alias of its last value.
    #[test]
    fn each_table_shape_gives_its_self_a_type() {
        let src = concat!(
            "local Plain = { }\nfunction Plain:m() end\n",
            "local Klass = { }\nKlass.__index = Klass\nfunction Klass:m() end\n",
            "local Cls = { }\nCls.__index = Cls\nfunction Cls.new(a, b) return setmetatable({ a = a }, Cls) end\nfunction Cls:m() end\n",
            "local Rebound = { }\nRebound = { other = 1 }\nfunction Rebound:m() end\n",
            "local Late = { }\nfunction Late:m() end\nLate = { }\n",
            "local Meta = { }\nsetmetatable(Meta, {})\nfunction Meta:m() end\n",
        );
        let types = self_types(src);

        assert_eq!(types.get("Plain"), Some(&SelfType::Table));
        assert_eq!(types.get("Klass"), Some(&SelfType::Instance(None)));
        assert_eq!(types.get("Cls"), Some(&SelfType::Instance(Some(2))));
        assert!(matches!(types.get("Rebound"), Some(SelfType::Rebound(_))));
        // A method before the rebind sits on the old table, and a
        // metatable of another shape says nothing of `self`.
        assert_eq!(types.get("Late"), None);
        assert_eq!(types.get("Meta"), None);

        assert_eq!(
            SelfType::Instance(Some(2)).text("Cls"),
            "typeof(Cls.new(nil :: any, nil :: any))"
        );
        assert_eq!(SelfType::Rebound(0).text("Rebound"), "__self_Rebound");
    }
}
