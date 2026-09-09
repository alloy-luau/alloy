//! A method hovered at the end of a call chain, or through a temp the
//! emit made: the receiver's type stands in for the chain, and a
//! temp's name drops out of the signature.

const SIGNATURE: &str = "function ";

/// A method hovered at the end of a call chain names the chain,
/// `function Iter.from(xs):map(f):collect(self: Iter<number>)`. The
/// receiver's type stands in for the chain. A chain wraps, so the head
/// runs over as many lines as the source did; the whole of it goes.
pub(crate) fn fold_call_receivers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;

    while let Some(i) = text[from..].find(SIGNATURE) {
        let at = from + i;
        let head = at + SIGNATURE.len();

        // A signature opens a line; `-> function` names a type instead.
        match (at == 0 || text[..at].ends_with('\n'))
            .then(|| fold_call_receiver_at(&text[at..]))
            .flatten()
        {
            Some((len, folded)) => {
                out.push_str(&text[from..at]);
                out.push_str(&folded);
                from = at + len;
            }

            None => {
                out.push_str(&text[from..head]);
                from = head;
            }
        }
    }

    out.push_str(&text[from..]);

    out
}

/// The head `function <receiver>:<name>(self: T` at the start of the
/// text: how far it runs, and the same head with the receiver replaced
/// by the name of `T`. `None` when the receiver is already a name, or
/// when the self type has none.
fn fold_call_receiver_at(text: &str) -> Option<(usize, String)> {
    // The head stops at the fence or the blank line that closes the
    // signature; a bracket left open past either is not part of it.
    let limit = [text.find("\n```"), text.find("\n\n")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let span = &text[..limit];
    let mut depth = 0i32;
    let mut colon = None;
    let mut open = None;

    for (k, c) in span[SIGNATURE.len()..].char_indices() {
        let k = k + SIGNATURE.len();

        match c {
            '(' if depth == 0 && span[k + 1..].starts_with("self: ") => {
                open = Some(k);

                break;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => colon = Some(k),
            _ => {}
        }
    }

    let open = open?;
    let colon = colon?;
    let receiver = &span[SIGNATURE.len()..colon];
    let after = &span[open + "(self: ".len()..];
    let name_len = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .count();

    if name_len == 0 {
        return None;
    }

    // A receiver that is a name already, `Iter:filter`, is the type.
    // `self:markup` and `x:describe` name the variable the call went
    // through; the type is what the reader wants there, and only a
    // type name stands in for it. The first word decides, so
    // `self: read number[]` leaves a plain receiver as it is.
    if !receiver.contains('(') && !after[..name_len].starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }

    let name = receiver_type_name(self_type(after))?;

    Some((open, format!("{SIGNATURE}{name}{}", &span[colon..open])))
}

/// The `self` annotation of a head: the text from the type to the comma
/// or the parenthesis that closes the parameter.
fn self_type(after: &str) -> &str {
    let mut depth = 0i32;

    for (i, c) in after.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return &after[..i],
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => return &after[..i],
            _ => {}
        }
    }

    after
}

/// The type name a `self` annotation stands for. A modifier is no
/// name, an array reads as `Array`, and a generic reads as its head.
fn receiver_type_name(ty: &str) -> Option<String> {
    let mut t = ty.trim();

    for word in ["read ", "write "] {
        t = t.strip_prefix(word).unwrap_or(t).trim_start();
    }

    let t = t.trim_end_matches('?').trim_end();

    if t.ends_with("[]") {
        return Some("Array".to_string());
    }

    let head: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    match head.is_empty() || matches!(head.as_str(), "any" | "unknown" | "nil" | "self") {
        true => None,

        false => Some(head),
    }
}

/// `function _1:unwrap(self: any): T`: the receiver is a temp the emit
/// made; the method reads without it.
pub(crate) fn fold_temp_receiver(text: &str) -> String {
    let mut out = text.to_string();

    while let Some(i) = out.find("function _") {
        let rest = &out[i + "function _".len()..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();

        if digits == 0 || !matches!(rest[digits..].chars().next(), Some(':' | '.')) {
            break;
        }

        out.replace_range(
            i + "function ".len()..i + "function _".len() + digits + 1,
            "",
        );
    }

    out
}
