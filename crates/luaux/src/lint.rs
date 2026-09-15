//! Checks on the Luau inside LuauX expressions (PROPOSAL.md Rules).
//!
//! Expressions are captured verbatim and never interpreted by the compiler, so
//! anything checked here is checked by re-parsing the captured text with
//! `full_moon`.

use crate::backend::EmitError;
use full_moon::ast::{BinOp, Expression};

/// Rule 2 — a literal `nil` as the left operand of `and`.
///
/// `cond and nil or x` **always** evaluates to `x` in Lua: `cond and nil` is
/// falsy whether `cond` holds or not, so `or x` always wins. Written as
/// conditional rendering it reads like a ternary and behaves like a constant.
///
/// Rejecting it keeps every other `and`/`or` faithful to Lua rather than
/// silently reinterpreting this one shape, which is what a "fix" would amount
/// to. Luau's `if ... then ... else` expression is the unambiguous form.
pub fn check_conditional_child(expression: &str, offset: usize) -> Result<(), EmitError> {
    let wrapped = format!("local _ = {expression}");
    let parsed = full_moon::parse_fallible(&wrapped, full_moon::LuaVersion::luau());

    let Some(expression_node) = first_expression(parsed.ast()) else {
        return Ok(());
    };

    if !has_nil_and_operand(expression_node) {
        return Ok(());
    }

    Err(EmitError::new(
        "`and nil or` always yields the right-hand side in Lua, so this condition has no effect",
        offset,
        expression.len() + 2,
    )
    .with_help("write `if cond then ... else ...` instead"))
}

fn first_expression(ast: &full_moon::ast::Ast) -> Option<&Expression> {
    ast.nodes().stmts().find_map(|statement| match statement {
        full_moon::ast::Stmt::LocalAssignment(assignment) => assignment.expressions().iter().next(),
        _ => None,
    })
}

/// Looks through parentheses for `<anything> and nil`, at any depth on the left
/// spine — `a and nil or b` parses as `(a and nil) or b`.
fn has_nil_and_operand(expression: &Expression) -> bool {
    match expression {
        Expression::Parentheses { expression, .. } => has_nil_and_operand(expression),
        Expression::BinaryOperator { lhs, binop, rhs } => {
            if matches!(binop, BinOp::And(_)) && is_nil_literal(rhs) {
                return true;
            }
            has_nil_and_operand(lhs) || has_nil_and_operand(rhs)
        }
        _ => false,
    }
}

/// The `static_conditional_child` warning (PLAN.md §11.1).
///
/// The suggestion is built from `[factory] compute`, because the fix is not the
/// same under every library. A bare function is what Vide tracks; under Fusion
/// a bare function is a value, and the reactive form is the configured wrapper.
/// Suggesting the wrong one sends someone to write code that silently does
/// nothing — which is what this lint exists to prevent.
pub fn static_conditional_child(
    offset: usize,
    length: usize,
    compute: Option<&str>,
) -> crate::compile::Warning {
    let help = match compute {
        Some(wrapper) => {
            format!("wrap it so the library tracks it: {{{wrapper}(function() return ... end)}}")
        }
        None => "wrap it in a function so the library tracks it: {function() return ... end}"
            .to_string(),
    };

    crate::compile::Warning {
        message: "this child is built once, so a condition around it will not update the UI"
            .to_string(),
        offset,
        length: length + 2,
        help: Some(help),
    }
}

/// Whether LuauX in this expression sits outside every function literal.
///
/// Such LuauX is constructed exactly once, so a condition around it looks live but
/// is not — the `static_conditional_child` lint (PLAN.md §11.1). LuauX *inside* a
/// function is fine: the library re-runs it, which is how reactive children
/// work, and it covers the idiomatic `items:map(function() return <Row/> end)`.
///
/// Works on the pre-compilation source, where LuauX is still `<...>`.
pub fn has_unwrapped_luaux(expression: &str, luaux_spans: &[(usize, usize)]) -> bool {
    if luaux_spans.is_empty() {
        return false;
    }

    let Ok(tokens) = crate::tokenize(expression) else {
        return false;
    };

    // Depth of `function ... end` nesting at each byte offset. Other `end`-taking
    // keywords are tracked too, since they share the terminator.
    let mut function_depth = 0i32;
    let mut block_depth = 0i32;
    let mut inside: Vec<(usize, i32)> = Vec::new();

    for token in &tokens {
        if token.is_trivia() {
            continue;
        }

        match token.text(expression) {
            "function" => {
                function_depth += 1;
                block_depth += 1;
            }
            "if" | "do" | "for" | "while" => block_depth += 1,
            "end" => {
                block_depth -= 1;
                // An `end` closing a function pops the function depth. Blocks
                // nest strictly, so the innermost opener is the one that closes.
                if function_depth > block_depth {
                    function_depth = block_depth.max(0);
                }
            }
            _ => {}
        }

        inside.push((token.start, function_depth));
    }

    luaux_spans.iter().any(|(start, _)| {
        let depth = inside
            .iter()
            .rev()
            .find(|(offset, _)| offset <= start)
            .map(|(_, depth)| *depth)
            .unwrap_or(0);
        depth == 0
    })
}

fn is_nil_literal(expression: &Expression) -> bool {
    match expression {
        Expression::Parentheses { expression, .. } => is_nil_literal(expression),
        Expression::Symbol(symbol) => symbol.token().to_string() == "nil",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(expression: &str) -> Result<(), EmitError> {
        check_conditional_child(expression, 0)
    }

    #[test]
    fn rejects_nil_on_the_left_of_and() {
        assert!(check("isExpensive and nil or label").is_err());
        assert!(check("(isExpensive and nil) or label").is_err());
        assert!(check("a and b and nil or c").is_err());
    }

    #[test]
    fn allows_the_faithful_ternary_shapes() {
        // These are correct in Lua because an element is always truthy.
        assert!(check("isExpensive and label or nil").is_ok());
        assert!(check("isExpensive and expensive or cheap").is_ok());
        assert!(check("if isExpensive then a else b").is_ok());
    }

    #[test]
    fn allows_ordinary_expressions() {
        assert!(check("items").is_ok());
        assert!(check("f(x, nil)").is_ok());
        assert!(check("function() return a and nil end").is_ok());
        assert!(check("t.nilable").is_ok());
    }

    fn unwrapped(expression: &str) -> bool {
        let spans = crate::compile::luaux_spans_for_test(expression);
        has_unwrapped_luaux(expression, &spans)
    }

    #[test]
    fn flags_luaux_outside_any_function() {
        assert!(unwrapped("cond() and <X/> or nil"));
        assert!(unwrapped("<X/>"));
        assert!(unwrapped("if cond then <X/> else nil"));
    }

    #[test]
    fn accepts_luaux_inside_a_function() {
        assert!(!unwrapped("function() return cond() and <X/> or nil end"));
        assert!(!unwrapped("items:map(function(i) return <Row/> end)"));
        assert!(!unwrapped("show(cond, function() return <X/> end)"));
    }

    #[test]
    fn accepts_expressions_with_no_luaux() {
        assert!(!unwrapped("items"));
        assert!(!unwrapped("cond and a or nil"));
    }

    #[test]
    fn tracks_nesting_through_blocks_that_share_end() {
        // `if ... end` inside a function must not pop the function depth early.
        assert!(!unwrapped(
            "function() if cond then return <X/> end return nil end"
        ));
        // ...and a function that has genuinely closed no longer shelters LuauX.
        assert!(unwrapped("f(function() return 1 end) and <X/> or nil"));
    }

    #[test]
    fn tolerates_expressions_it_cannot_parse() {
        // Never the source of a spurious error; other passes report bad syntax.
        assert!(check("!!!").is_ok());
    }
}
