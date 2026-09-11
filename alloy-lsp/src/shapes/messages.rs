//! A checker message reworded for a reader: what names only the emit,
//! what a mapped print says once folded, and the sentence a solver
//! failure or a step-limit report reads as.

/// Whether a message names something the reader never wrote, so it says
/// nothing they can act on.
///
/// `%error-id%` is the checker's stand-in for a name a half-typed member
/// access has yet to give. The parser already reports the missing name.
pub fn names_only_the_emit(message: &str) -> bool {
    // The checker names the two emitted files of an import cycle;
    // `circular_import` names the two the author wrote.
    // The require binding as the subject of a report: the reader never
    // wrote the name, and the mistake reads on the annotation instead.
    message.contains("%error-id%")
        || message.contains("Cyclic module dependency")
        || message.contains("'__alloy'")
}

/// Whether a report is about a key the emit writes and the source line
/// does not: an enum's `tag`, and the `_1`, `_2` its payload goes in.
/// A reader who never wrote the name has nothing to fix.
pub fn names_the_emit_key(message: &str, line: &str) -> bool {
    const OPENERS: [&str; 3] = ["does not have key '", "Key '", "Cannot add property '"];

    // `await` reads a Future's payload through `__value`, a key the
    // type carries and no source writes. The report beside it already
    // names what the reader wrote.
    if message.contains("__value") && !holds_word(line, "__value") {
        return true;
    }

    OPENERS
        .iter()
        .filter_map(|opener| {
            let at = message.find(opener)? + opener.len();

            message[at..].find('\'').map(|end| &message[at..at + end])
        })
        .any(|key| {
            let emitted = key == "tag"
                || key
                    .strip_prefix('_')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));

            emitted && !holds_word(line, key)
        })
}

/// Whether a duplicate-field report is about a table the emit built:
/// the source line writes the key once, or not at all. A markup
/// attribute that expands to several properties makes these.
pub fn duplicate_only_in_the_emit(message: &str, line: &str) -> bool {
    const OPENER: &str = "Table field '";

    let Some(at) = message.find(OPENER).map(|i| i + OPENER.len()) else {
        return false;
    };
    let Some(end) = message[at..].find('\'') else {
        return false;
    };

    if !message.contains("is a duplicate") {
        return false;
    }

    let key = &message[at..at + end];

    line.match_indices(key)
        .filter(|(at, _)| {
            !line[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                && !line[at + key.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .count()
        < 2
}

/// Whether a line holds a name as a whole word.
fn holds_word(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(at, _)| {
        !line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
            && !line[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    })
}

/// A `{ ... }` where an Array belongs, as the mistake reads. The checker
/// answers with the nineteen methods the table lacks; the reader wrote
/// the wrong bracket.
pub fn plain_table_hint(message: &str) -> Option<String> {
    const HEAD: &str = "Table type '";
    const MIDDLE: &str = "' not compatible with type '";

    if !message.contains("missing fields") {
        return None;
    }

    let at = message.find(HEAD)? + HEAD.len();
    let mid = message[at..].find(MIDDLE)? + at;
    let after = mid + MIDDLE.len();
    let end = message[after..].find('\'')? + after;
    let want = &message[after..end];
    let named = want.trim_end_matches('?');

    if !(named.ends_with("[]") || named.starts_with("Array<")) {
        return None;
    }

    Some(format!(
        "a `{{ ... }}` is a plain table, not a `{want}`; an Array literal is `[ ... ]`"
    ))
}

/// A checker message as a reader should get it: a failed bound reads as
/// a bound, and the tail that walks the emitted shape goes.
pub fn friendly_text(message: &str) -> String {
    let text = bound_failure(message)
        .or_else(|| pack_mismatch(message))
        .or_else(|| unsolved_generic(message))
        .or_else(|| solver_gave_up(message))
        .unwrap_or_else(|| cut_explanation(message));
    let text = without_import_temp(&text);

    table_beside_array(&text).unwrap_or(text)
}

/// The local an import emits, `_m1`, in front of a name the message
/// prints: `Unknown type '_m1.x'`. The reader wrote `x` on the import
/// line and never saw the local, so the path in front of it goes.
fn without_import_temp(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < text.len() {
        let rest = &text[at..];
        let temp = rest.strip_prefix("_m").map(|r| {
            let digits = r.chars().take_while(char::is_ascii_digit).count();

            (digits, r[digits..].starts_with('.'))
        });
        let starts_a_word = at == 0 || !(bytes[at - 1] as char).is_alphanumeric();

        match temp {
            Some((digits, true)) if digits > 0 && starts_a_word => at += 2 + digits + 1,

            _ => {
                let ch = rest.chars().next().unwrap_or('\0');
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }

    out
}

/// The checker's own step limit, worded as an order to the reader. It
/// says nothing is wrong with the code, only that the checker stopped.
fn solver_gave_up(message: &str) -> Option<String> {
    const CLAUSE: &str = "Code is too complex to typecheck!";

    let at = message.find(CLAUSE)?;

    Some(format!(
        "{}the checker reached its limit on this expression; it says nothing about the code. Name a step in a local, or annotate the result",
        &message[..at]
    ))
}

/// A generic the checker could not solve. It answers with the bounds it
/// collected, which name no place and no fix; the reader wants to know
/// that the values do not agree.
fn unsolved_generic(message: &str) -> Option<String> {
    const CLAUSE: &str = "No valid instantiation could be inferred for generic type parameter ";

    let at = message.find(CLAUSE)? + CLAUSE.len();
    let name: String = message[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let head = match message.split_once(": ") {
        Some((kind, _)) if !kind.contains(' ') => format!("{kind}: "),

        _ => String::new(),
    };

    (!name.is_empty()).then(|| {
        format!(
            "{head}these values give `{name}` no one type; make them agree, or write `{name}` out"
        )
    })
}

/// `{T}` is a plain Luau table and `T[]` is an Array with its methods.
/// A message that holds both reads as one type printed two ways, so it
/// says which is which.
pub fn table_beside_array(message: &str) -> Option<String> {
    let name = message.match_indices('{').find_map(|(at, _)| {
        let rest = message.get(at + 1..)?.trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name.is_empty() || !rest[name.len()..].trim_start().starts_with('}') {
            return None;
        }

        message.contains(&format!("{name}[]")).then_some(name)
    })?;

    Some(format!(
        "{message}; a `{{ {name} }}` is a plain table, and `{name}[]` is an Array"
    ))
}

/// A callback whose parameters do not line up. The checker explains it
/// by walking the type pack, which reads as a broken sentence and calls
/// the two types "former" and "latter". The parameter, what the source
/// wrote, and what the callee asks for say it.
fn pack_mismatch(message: &str) -> Option<String> {
    const CLAUSE: &str = "entry in the type pack is ";

    let flat = flatten(message);
    let at = flat.find(CLAUSE)?;
    let ordinal = flat[..at].split_whitespace().next_back()?.to_string();

    if !ordinal.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    let rest = &flat[at + CLAUSE.len()..];
    let first = quoted_from(rest)?;
    let after = rest.get(first.len() + 2..)?;
    let second = quoted_from(after.split_once("type and ")?.1)?;
    // "the latter type" is the type the source wrote; "the former" is
    // the one the callee asks for.
    let (got, want) = match after.trim_start().starts_with("in the latter") {
        true => (first, second),

        false => (second, first),
    };
    let head = flat[..at].split_once("; it ").map(|(h, _)| h)?.trim_end();

    // A pack of parameters belongs to a function on both sides; any
    // other pack keeps the head alone.
    if head.matches("->").count() < 2 {
        return Some(head.to_string());
    }

    Some(format!(
        "{head}: its {ordinal} parameter is `{got}` where `{want}` is wanted"
    ))
}

/// One line of a message the checker laid out over several, with its
/// tabs and its runs of spaces closed up.
fn flatten(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `where T: Shape` emits as an intersection, so a bound the argument
/// misses reads as an intersection it is not part of.
fn bound_failure(message: &str) -> Option<String> {
    const CLAUSE: &str = "component of the intersection is ";

    let at = message.find(CLAUSE)? + CLAUSE.len();
    let bound = quoted_from(&message[at..])?;
    let got_at = message.find("but got ")? + "but got ".len();
    let got = quoted_from(&message[got_at..])?;

    // A bound the argument misses sits in the type the checker WANTED.
    // A `Result` is itself an intersection, so the same clause also
    // walks the type it GOT, and rewriting that reads as a type that
    // does not satisfy itself. The top line already names both sides,
    // so it stands instead.
    if got.contains(bound) {
        return None;
    }
    // The message keeps the kind it came with; a caller that adds one
    // would print it twice.
    let head = match message.split_once(": ") {
        Some((kind, _)) if !kind.contains(' ') => format!("{kind}: "),

        _ => String::new(),
    };

    Some(format!(
        "{head}`{got}` does not satisfy the bound `{bound}`"
    ))
}

/// The text the quote at the start of a message fragment opens; the
/// checker writes either a quote or a backtick.
fn quoted_from(text: &str) -> Option<&str> {
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '`'))?;
    let end = text[1..].find(quote)?;

    Some(&text[1..1 + end])
}

/// The checker explains a mismatch by walking the shape it printed, so
/// the tail names the emit: `_1`, `__index`, and the type pack. The
/// head already says what the reader needs.
fn cut_explanation(message: &str) -> String {
    const MARKERS: [&str; 2] = ["this is because", "in the metatable portion"];

    let Some(at) = MARKERS.iter().filter_map(|m| message.find(m)).min() else {
        return message.to_string();
    };
    let head = message[..at].trim_end();

    head.strip_suffix(';')
        .unwrap_or(head)
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_mismatch_names_the_parameter_and_not_the_type_pack() {
        let text = "Expected this to be '(number, number) -> string' but got '(string) -> string'; it takes the 1st entry in the type pack is `string` in the latter type and `number` in the former type, and `string` is not a supertype of `number`";
        assert_eq!(
            friendly_text(text),
            "Expected this to be '(number, number) -> string' but got '(string) -> string': its 1st parameter is `string` where `number` is wanted"
        );
    }

    #[test]
    fn the_same_sentence_reads_alike_over_several_lines() {
        let text = "Expected this to be\n\t'(Player, number) -> ()'\nbut got\n\t'(Player, string) -> ()'; \nit takes the 2nd entry in the type pack is `string` in the latter type and `number` in the former type";
        assert_eq!(
            friendly_text(text),
            "Expected this to be '(Player, number) -> ()' but got '(Player, string) -> ()': its 2nd parameter is `string` where `number` is wanted"
        );
    }

    #[test]
    fn a_report_about_a_key_the_emit_writes_is_dropped() {
        let line = "local Build(first_name) = Job.Build(\"tower\")";
        assert!(names_the_emit_key(
            "Type 'nil' does not have key 'tag'",
            line
        ));
        assert!(names_the_emit_key(
            "Key '_1' not found in table 'Job'",
            line
        ));
        // The source that writes the name keeps its report.
        assert!(!names_the_emit_key(
            "Type 'Row' does not have key 'tag'",
            "print(row.tag)"
        ));
        assert!(!names_the_emit_key(
            "Type 'Row' does not have key 'name'",
            line
        ));
    }

    #[test]
    fn a_duplicate_the_lowering_made_is_dropped() {
        let expanded = "            <Frame Name=\"Stats\" ClassName=\"panel bg-orange-700\">";
        assert!(duplicate_only_in_the_emit(
            "Table field 'BackgroundColor3' is a duplicate; previously defined at line 65",
            expanded
        ));
        assert!(!duplicate_only_in_the_emit(
            "Table field 'hp' is a duplicate; previously defined at line 4",
            "local t = { hp = 1, hp = 2 }"
        ));
    }

    #[test]
    fn a_table_beside_an_array_says_which_is_which() {
        assert_eq!(
            friendly_text("Expected this to be 'Future<{Profile}>', but got 'Future<Profile[]>'"),
            "Expected this to be 'Future<{Profile}>', but got 'Future<Profile[]>'; a `{ Profile }` is a plain table, and `Profile[]` is an Array"
        );
        assert_eq!(
            friendly_text("Expected this to be 'number', but got 'string'"),
            "Expected this to be 'number', but got 'string'"
        );
    }

    #[test]
    fn a_generic_with_no_solution_reads_as_the_values_that_disagree() {
        assert_eq!(
            friendly_text(
                "TypeError: No valid instantiation could be inferred for generic type parameter T. It was expected to be at least: number | nil and at most: number & nil but these types are not compatible with one another."
            ),
            "TypeError: these values give `T` no one type; make them agree, or write `T` out"
        );
    }

    #[test]
    fn the_checkers_step_limit_says_it_is_the_checker() {
        assert_eq!(
            friendly_text(
                "TypeError: Code is too complex to typecheck! Consider simplifying the code around this area"
            ),
            "TypeError: the checker reached its limit on this expression; it says nothing about the code. Name a step in a local, or annotate the result"
        );
    }

    #[test]
    fn a_pack_that_is_no_callback_keeps_the_head_alone() {
        let text = "Expected this to be 'number' but got 'string'; it takes the 1st entry in the type pack is `string` in the latter type and `number` in the former type";
        assert_eq!(
            friendly_text(text),
            "Expected this to be 'number' but got 'string'"
        );
    }
}

#[cfg(test)]
mod import_temp_tests {
    use super::*;

    /// `import type { x }` of a value read `Unknown type '_m1.x'`. The
    /// local is the emit's; the reader wrote `x`.
    #[test]
    fn a_message_drops_the_import_local() {
        assert_eq!(
            friendly_text("TypeError: Unknown type '_m1.x'"),
            "TypeError: Unknown type 'x'"
        );
        assert_eq!(
            friendly_text("TypeError: Unknown type '_m12.Vec2'"),
            "TypeError: Unknown type 'Vec2'"
        );

        // A name of the reader's that begins the same way stays whole.
        assert_eq!(
            friendly_text("TypeError: Unknown type 'x_m1.y'"),
            "TypeError: Unknown type 'x_m1.y'"
        );
        assert_eq!(
            friendly_text("TypeError: Unknown type '_market'"),
            "TypeError: Unknown type '_market'"
        );
    }

    /// A `where T: Shape` the argument misses: the bound sits in the
    /// type the checker wanted, so the sentence names both sides.
    #[test]
    fn a_missed_bound_reads_as_a_bound() {
        let raw = "TypeError: Expected this to be 'Plain & Named', but got 'Plain'; \
                   this is because the 2nd component of the intersection is `Named`, \
                   which is not a subtype of `Plain`";

        assert_eq!(
            friendly_text(raw),
            "TypeError: `Plain` does not satisfy the bound `Named`"
        );
    }

    /// A `Result` is itself an intersection, so the same clause walks
    /// the type the checker GOT. Rewriting that read as a type that
    /// does not satisfy itself, and threw away a top line that already
    /// named both sides.
    #[test]
    fn an_intersection_inside_the_got_type_is_no_bound() {
        let raw = "TypeError: Expected this to be 'number', but got '(A | B) & ResultMethods2<string, any>'; \
                   this is because * the 2nd component of the intersection is `ResultMethods2<string, any>`, \
                   which is not a subtype of `number`";

        let out = friendly_text(raw);

        assert!(
            out.starts_with("TypeError: Expected this to be 'number', but got "),
            "the top line stands: {out}"
        );
        assert!(!out.contains("does not satisfy the bound"), "{out}");
    }
}
