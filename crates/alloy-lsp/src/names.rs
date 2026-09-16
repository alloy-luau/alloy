/*!
What a position offers a reader: the globals a value expression reaches,
whether the caret sits on a name the author declares, and what a
built-in attribute goes on.

The playground includes this file, so the editor and the page answer the
same. The file holds no state and reads nothing outside itself.
*/

/// The globals a value expression reaches for, for the list the proxy
/// builds where luau-lsp answers nothing. The full global list is the
/// child's to give; these are the names an arm or a ternary writes.
pub(crate) const EXPRESSION_GLOBALS: &[&str] = &[
    "print",
    "warn",
    "error",
    "assert",
    "tostring",
    "tonumber",
    "typeof",
    "type",
    "ipairs",
    "pairs",
    "next",
    "select",
    "pcall",
    "math",
    "string",
    "table",
    "os",
    "task",
    "buffer",
    "coroutine",
    "utf8",
    "game",
    "workspace",
    "script",
    "Instance",
    "Enum",
    "Vector3",
    "Vector2",
    "CFrame",
    "Color3",
    "UDim",
    "UDim2",
    "TweenInfo",
    "BrickColor",
    "Random",
    "NumberRange",
    "DateTime",
];

/// Whether `offset` sits in a name a declaring keyword introduces: the
/// word before the one at the cursor is `enum`, `struct`, `function`,
/// `local`, and the rest. The name is the author's, so no list belongs
/// there, at the first column of the name as much as mid-word.
///
/// `impl` and `class` take a type, not a new name, and an `import`
/// names nothing of its own but the alias of `* as M`.
pub(crate) fn declares_a_name_at(source: &str, offset: usize) -> bool {
    const DECLARERS: &[&str] = &[
        "enum",
        "struct",
        "trait",
        "interface",
        "type",
        "function",
        "local",
        "const",
        "macro",
        "attribute",
        "remote",
        "namespace",
    ];
    let offset = offset.min(source.len());
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = offset;

    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }

    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let statement = source[line_start..offset].trim_start();
    let import = statement
        .strip_prefix("import")
        .is_some_and(|rest| !rest.starts_with(|c: char| is_word(c as u8)));

    // Every name in an `import` comes from the module; the alias of
    // `* as M` and a default binding are the author's own.
    if import {
        let head = source[line_start..start].trim_end();

        if head
            .strip_suffix("as")
            .is_some_and(|h| h.trim_end().ends_with('*'))
        {
            return true;
        }

        let mut word_end = start;

        while word_end < bytes.len() && is_word(bytes[word_end]) {
            word_end += 1;
        }

        let after_keyword = head
            .trim_start()
            .strip_prefix("import")
            .map(str::trim)
            .unwrap_or("-");

        return after_keyword.is_empty()
            && word_end > start
            && source[word_end..].trim_start().starts_with("from");
    }

    // A clause of an attribute contract names a member the declaration
    // must carry, not a new name: `requires private function |` takes
    // `each`, which the contract list answers with.
    if statement.starts_with("requires ") {
        return false;
    }

    let mut end = start;

    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }

    if end == start {
        return false;
    }

    let mut word_start = end;

    while word_start > 0 && is_word(bytes[word_start - 1]) {
        word_start -= 1;
    }

    DECLARERS.contains(&&source[word_start..end])
}

/// What a built-in attribute goes on. The list mirrors
/// `builtin_attr_targets` in the compiler, which is what reports an
/// attribute on the wrong declaration.
pub(crate) fn builtin_attribute_targets(key: &str) -> &'static [&'static str] {
    match key {
        "@derive" | "@sealed" => &["struct", "enum"],

        "@cfg" => &["function", "local", "namespace"],

        "@deprecated" => &["function", "namespace"],

        // `@test` on a namespace makes every public function of the
        // group a test, nested public namespaces included.
        "@test" => &["function", "namespace"],

        "@native" | "@checked" | "@inline" | "@noinline" => &["function"],

        "@unreliable" | "@ratelimit" | "@timeout" | "@validate" | "@immediate" => &["remote"],

        "@u8" | "@u16" | "@u32" | "@i8" | "@i16" | "@i32" | "@f32" => &["param", "field"],

        "@rename" | "@skip" => &["field"],

        _ => &[
            "function",
            "struct",
            "enum",
            "variant",
            "field",
            "param",
            "remote",
            "interface",
            "type",
            "local",
            "namespace",
            "impl",
            "trait",
        ],
    }
}
