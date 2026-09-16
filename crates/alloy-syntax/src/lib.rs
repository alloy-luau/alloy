/*!
alloy_syntax: a small, fast Luau syntax layer.

The crate holds the lexer, the parser, the AST, the require-site scanner,
the lossless printer, and the dense re-emitter that larvae builds on. It
depends on nothing of larvae, so any tool can parse Luau with it.

Byte ranges are the identity of everything here. Every token and every node
carries byte offsets into the source, never a line and a column; a consumer
derives those on demand. The printer replays the tokens with the source
between them, so a tree without changes reproduces its input byte for byte,
and a transform can splice byte ranges against the original text with no
loss. These two properties are the contract of the crate, and the fuzz
target holds the round trip.

Parallelism is file level. One file parses fast and single threaded, so a
caller that holds many files spreads them itself.
*/

pub mod ast;
pub mod contextual;
pub mod lexer;
pub mod parser;
pub mod printer;
pub mod scan;
pub mod stand_in;

/// The words an `attribute ... on ...` clause accepts.
///
/// The parser and the editor read this one list, so the completion
/// cannot fall behind the grammar.
pub const ATTRIBUTE_TARGETS: &[&str] = &[
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
];

/// One parsed file: its tokens with the comment spans, and its tree
pub struct Parsed {
    pub lexed: lexer::Lexed,
    pub chunk: ast::Chunk,
}

/// The failure of one file, with the byte offset where it happened
pub struct ParseFailure {
    pub offset: usize,
    pub message: String,
}

/// Lex and parse one source
pub fn parse_one(src: &str) -> Result<Parsed, ParseFailure> {
    let lexed = lexer::lex(src).map_err(|e| ParseFailure {
        offset: e.offset,
        message: e.message,
    })?;

    let chunk = parser::parse(src, &lexed.toks).map_err(|e| ParseFailure {
        offset: e.offset,
        message: e.message,
    })?;

    Ok(Parsed { lexed, chunk })
}

/// One file parsed leniently: the tree covers every token, and each
/// stretch the parser could not read is an error node with a diagnostic.
pub struct Lenient {
    pub lexed: lexer::Lexed,
    pub chunk: ast::Chunk,
    pub diagnostics: Vec<parser::ParseError>,
}

/// Lex and parse one source, recovering from errors. A lex error still
/// fails, because the lexer defines what a token is.
pub fn parse_lenient(src: &str, options: parser::ParseOptions) -> Result<Lenient, ParseFailure> {
    let lexed = lexer::lex(src).map_err(|e| ParseFailure {
        offset: e.offset,
        message: e.message,
    })?;

    let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, options);

    Ok(Lenient {
        lexed,
        chunk,
        diagnostics,
    })
}
