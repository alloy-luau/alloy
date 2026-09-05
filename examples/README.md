# Alloy examples

One file per feature area. Each file is valid Alloy as designed in
`DESIGN.md`. The compiler does not run yet, so nothing here compiles today.
The files pin the syntax so the parser has a target.

| File                          | Shows                                                     |
|-------------------------------|-----------------------------------------------------------|
| `01_safe_access.aly`          | `??`, `??=`, `?.`, `?:`, `?[`, `->`, `=>`, `!`, chain rule |
| `02_modules/`                 | `export`, `import`, `import type`, `export default`, `@alias` |
| `03_async.aly`                | `async`, `await`, Future combinators, awaitable protocol   |
| `04_result.aly`               | `Result`, `try`, `Result.pcall`, `unwrap`, `try await`     |
| `05_enums_match.aly`          | payload enums, unit enums, `match`, guards, `impl` on enum |
| `06_conditional_bindings.aly` | `if local`, `not`, patterns, `while local`, let-else         |
| `07_intrinsics.aly`           | `$dbg`, `$todo`, `$unreachable`, `$assert`, `$nameof`      |
| `08_structs_traits.aly`       | `struct`, `impl`, `trait`, derive, operator traits, bounds |
| `09_interfaces_types.aly`     | `interface extends`, mapped types                          |
| `10_declarations.aly`         | `const`, destructuring, `new`, `delete`, Drop              |
| `11_ui.alx`                   | JSX on luaux with Alloy syntax inside expression slots     |
| `22_app.aly`                  | An .aly file imports the .alx component; both directions resolve |
| `12_globals.d.aly`            | declaration file: `declare`, interfaces, traits, no runtime |
| `13_std.aly`                  | ambient std: `[ ]` arrays, `T[]`, `Array`, `HashMap`, `Set`, `Future` |
| `14_oop.aly`                  | a class is `struct` + `impl`; traits for shared behavior; composition |
| `15_sugar.aly`                | default params, method refs, spread, `satisfies`, bitwise words, filters, `new X() { }` |
| `16_remotes.aly`              | typed remotes: declaration, attributes, server and client surfaces |
| `17_tests.aly`                | `@test` functions and the assertion intrinsics                     |
| `18_extensions.aly`           | extension impls on foreign types, called with `:` and `.`         |
| `19_patterns.aly`             | nested, or, struct, table, and array patterns                       |
| `20_macros.aly`               | user macros with `$name(...)`, hygiene, and the non-nil `!`               |
| `21_attributes.aly`           | user-defined attributes, placement, typed args, reading them at runtime |

Every Alloy block closes with `end`. `as` follows the name of a `struct`,
`enum`, or `interface`. `match` uses `match x with case Pat then ...
default ... end`, and `derive` is an attribute on the line before the
struct: `@derive(Eq, Debug, Clone)`.
