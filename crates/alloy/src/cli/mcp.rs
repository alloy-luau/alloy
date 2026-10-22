//! `alloy mcp`: the docs of `alloy doc` for a coding agent, over MCP.
//!
//! The server reads JSON-RPC 2.0 from stdin, one message per line, and
//! writes one reply per request to stdout. Nothing else goes to stdout,
//! because the client reads every line there as a message.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use alloy::docs::{STYLE, TABLE};
use alloy::lint::LINTS;
use serde_json::{Value, json};

use crate::cli::doc;
use crate::fail;

/// The protocol versions this server speaks. A tools-only server reads
/// the same in each.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The version the server answers when the client asks for one it
/// does not know.
const LATEST: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Alloy is a strict superset of Luau that compiles each file to plain Luau on the same lines. Call style_guide before you write Alloy code. Call search_docs or list_topics to find a topic, and read_doc to read it. The topics are the ones `alloy doc` prints.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// The most hits `search_docs` returns, and the count it returns when
/// the call names none.
const MAX_LIMIT: u64 = 50;
const DEFAULT_LIMIT: u64 = 10;

pub(crate) fn run() -> ExitCode {
    match serve(std::io::stdin().lock(), std::io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,

        Err(e) => {
            fail(&format!("mcp: {e}"));

            ExitCode::FAILURE
        }
    }
}

/// Answers each line of `input` on `output` until the input ends.
fn serve(mut input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    let mut line = Vec::new();

    loop {
        line.clear();

        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }

        let reply = match std::str::from_utf8(&line) {
            Ok(text) if text.trim().is_empty() => continue,

            Ok(text) => handle(text),

            Err(_) => Some(error(Value::Null, PARSE_ERROR, "the line is not UTF-8")),
        };

        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
}

/// The reply to one line, or none for a notification and for a
/// response the client sends.
fn handle(line: &str) -> Option<Value> {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Some(error(Value::Null, PARSE_ERROR, "Parse error"));
    };
    let Some(object) = message.as_object() else {
        return Some(error(Value::Null, INVALID_REQUEST, "Invalid Request"));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // A response to a request of ours. The server sends none, so
        // it has nothing to match it to.
        if object.contains_key("result") || object.contains_key("error") {
            return None;
        }

        return Some(error(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "Invalid Request",
        ));
    };

    // A notification, `notifications/initialized` among them, gets no
    // reply.
    let id = id?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(initialize(&params)),

        "ping" => Ok(json!({})),

        "tools/list" => Ok(json!({ "tools": tools() })),

        "tools/call" => call(&params),

        _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),

        Err((code, message)) => error(id, code, &message),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = asked.filter(|v| VERSIONS.contains(v)).unwrap_or(LATEST);

    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "alloy", "version": alloy::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let none = json!({ "type": "object", "properties": {} });

    json!([
        {
            "name": "search_docs",
            "description": "Rank the `alloy doc` topics by matches of the query in the topic name and the body. Returns each topic with a short snippet.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The words to find, such as `optional chaining` or `remote`." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "The most topics to return. The default is 10." },
                },
                "required": ["query"],
            },
        },
        {
            "name": "read_doc",
            "description": "Return one topic as markdown: the text `alloy doc <topic>` prints.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topic": { "type": "string", "description": "A name from list_topics or search_docs, such as `match`, `?.`, `strict`, or `HashMap:get`." },
                },
                "required": ["topic"],
            },
        },
        {
            "name": "list_topics",
            "description": "List every topic of `alloy doc`, by group: the articles, the keywords, the operators, the std, and the lints.",
            "inputSchema": none,
        },
        {
            "name": "style_guide",
            "description": "Return the style guide: how to write Alloy that reads as Alloy, not as Luau with Alloy syntax. Read it before you write Alloy code.",
            "inputSchema": none,
        },
    ])
}

/// `tools/call`. An unknown tool is a protocol error. A bad argument or
/// an unknown topic is a tool error, which the model reads and can fix.
fn call(params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];

    let outcome = match name {
        "search_docs" => search_tool(args),

        "read_doc" => match args["topic"].as_str() {
            Some(topic) => doc::markdown(topic.trim()).ok_or_else(|| {
                format!(
                    "no doc for `{topic}`; list_topics lists the topics, and search_docs finds one"
                )
            }),

            None => Err("read_doc takes `topic`, a string".to_string()),
        },

        "list_topics" => Ok(list_topics()),

        "style_guide" => Ok(STYLE.to_string()),

        _ => return Err((INVALID_PARAMS, format!("Unknown tool: {name}"))),
    };

    let (text, failed) = match outcome {
        Ok(text) => (text, false),

        Err(text) => (text, true),
    };

    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": failed }))
}

fn search_tool(args: &Value) -> Result<String, String> {
    let query = args["query"]
        .as_str()
        .filter(|q| !q.trim().is_empty())
        .ok_or("search_docs takes `query`, a string with at least one word")?;
    let limit = match &args["limit"] {
        Value::Null => DEFAULT_LIMIT,

        value => value
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("`limit` is a whole number from 1 to {MAX_LIMIT}"))?
            .min(MAX_LIMIT),
    };
    let hits = search(query, limit as usize);

    if hits.is_empty() {
        return Ok(format!(
            "No topic matches `{query}`. list_topics lists every topic."
        ));
    }

    let mut out = format!("{} topics match `{query}`, best first:\n", hits.len());

    for (name, snippet) in hits {
        out.push_str(&format!("\n## {name}\n{snippet}\n"));
    }

    out.push_str("\nread_doc returns the full text of a topic.\n");

    Ok(out)
}

/// One topic: the name `read_doc` takes, its group in the index of
/// `alloy doc`, and the text a search reads.
struct Topic {
    name: String,
    group: &'static str,
    body: String,
}

/// Every topic: the entries of the table, with `topic:` cut from an
/// article, then the lints.
fn topics() -> Vec<Topic> {
    let entries = TABLE.iter().map(|(key, text)| Topic {
        name: key.strip_prefix("topic:").unwrap_or(key).to_string(),
        group: doc::group_of(key),
        body: (*text).to_string(),
    });
    let lints = LINTS.iter().map(|l| Topic {
        name: l.name.to_string(),
        group: "Lints",
        body: doc::lint_markdown(l),
    });

    entries.chain(lints).collect()
}

fn list_topics() -> String {
    let all = topics();
    let mut groups: Vec<&str> = Vec::new();

    for t in &all {
        if !groups.contains(&t.group) {
            groups.push(t.group);
        }
    }

    let mut out = String::from("Every topic read_doc takes, by group.\n");

    for group in groups {
        let mut names: Vec<&str> = all
            .iter()
            .filter(|t| t.group == group)
            .map(|t| t.name.as_str())
            .collect();
        names.sort_unstable();
        out.push_str(&format!("\n## {group}\n{}\n", names.join(", ")));
    }

    out
}

/// The topics a query matches, best first, each with a snippet. A word
/// in the name counts more than a word in the body, and the whole query
/// as the name counts most. The body count stops at 10, so a long
/// article does not win on length alone.
// ponytail: substring counts, no stemming; add an index if the table grows past a few thousand entries
fn search(query: &str, limit: usize) -> Vec<(String, String)> {
    let whole = query.trim().to_ascii_lowercase();
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();

    let mut hits: Vec<(usize, Topic, Option<usize>)> = topics()
        .into_iter()
        .filter_map(|t| {
            let name = t.name.to_ascii_lowercase();
            // ASCII lowercasing keeps every byte offset, so a match in
            // `body` is a match at the same place in `t.body`.
            let body = t.body.to_ascii_lowercase();
            let mut score = if name == whole { 100 } else { 0 };
            let mut first: Option<usize> = None;

            for word in &words {
                if name.contains(word.as_str()) {
                    score += 20;
                }

                score += body.matches(word.as_str()).count().min(10);

                if let Some(at) = body.find(word.as_str()) {
                    first = Some(first.map_or(at, |f| f.min(at)));
                }
            }

            (score > 0).then_some((score, t, first))
        })
        .collect();

    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));

    hits.into_iter()
        .take(limit)
        .map(|(_, t, at)| {
            let snippet = snippet(&t.body, at.unwrap_or(0));

            (t.name, snippet)
        })
        .collect()
}

/// About 200 characters of `body` around byte `at`, on one line.
fn snippet(body: &str, at: usize) -> String {
    let mut start = at.saturating_sub(60);
    let mut end = (at + 140).min(body.len());

    while !body.is_char_boundary(start) {
        start -= 1;
    }

    while !body.is_char_boundary(end) {
        end += 1;
    }

    let text = body[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let before = if start > 0 { "..." } else { "" };
    let after = if end < body.len() { "..." } else { "" };

    format!("{before}{text}{after}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `lines` through the server and parses each reply.
    fn session(lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n") + "\n";
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output).expect("the session");

        String::from_utf8(output)
            .expect("UTF-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect()
    }

    fn call_line(id: u64, name: &str, arguments: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }).to_string()
    }

    /// The handshake, a ping, and the tool list. A notification gets no
    /// reply, so three requests give three lines.
    #[test]
    fn the_handshake_lists_the_four_tools() {
        let replies = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":"two","method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        ]);

        assert_eq!(replies.len(), 3, "{replies:?}");

        let init = &replies[0];
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(init["result"]["serverInfo"]["name"], "alloy");
        assert_eq!(init["result"]["serverInfo"]["version"], alloy::VERSION);
        assert!(init["result"]["instructions"].is_string());

        assert_eq!(
            replies[1],
            json!({ "jsonrpc": "2.0", "id": "two", "result": {} })
        );

        let names: Vec<&str> = replies[2]["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(
            names,
            ["search_docs", "read_doc", "list_topics", "style_guide"]
        );
    }

    /// A version the server does not know gets the one it does.
    #[test]
    fn an_unknown_version_gets_the_latest() {
        let replies = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        ]);

        assert_eq!(replies[0]["result"]["protocolVersion"], LATEST);
    }

    /// Bad JSON, an unknown method, an unknown tool, and a bad argument
    /// each get their own error, and the session goes on after them.
    #[test]
    fn errors_answer_and_the_session_goes_on() {
        let replies = session(&[
            "{not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
            &call_line(2, "nope", json!({})),
            &call_line(3, "read_doc", json!({ "topic": "no-such-topic" })),
            &call_line(
                4,
                "search_docs",
                json!({ "query": "match", "limit": "ten" }),
            ),
            "[]",
            r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#,
        ]);

        assert_eq!(replies.len(), 7, "{replies:?}");
        assert_eq!(replies[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[1]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(replies[1]["id"], 1);
        assert_eq!(replies[2]["error"]["code"], INVALID_PARAMS);

        for reply in &replies[3..5] {
            assert_eq!(reply["result"]["isError"], true, "{reply}");
            assert_eq!(reply["result"]["content"][0]["type"], "text");
        }

        assert_eq!(replies[5]["error"]["code"], INVALID_REQUEST);
        assert_eq!(replies[6]["result"], json!({}));
    }

    /// `read_doc` returns the article `alloy doc` prints, as markdown,
    /// and `style_guide` returns the style article.
    #[test]
    fn the_tools_return_the_doc_text() {
        let replies = session(&[
            &call_line(1, "read_doc", json!({ "topic": "strict" })),
            &call_line(2, "style_guide", json!({})),
            &call_line(3, "read_doc", json!({ "topic": "prefer_new" })),
            &call_line(4, "list_topics", json!({})),
        ]);
        let text = |i: usize| {
            replies[i]["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
        };

        assert!(
            text(0).starts_with("# strict\n\n**Strict by default**"),
            "{}",
            text(0)
        );
        assert_eq!(text(1), STYLE);
        assert!(text(1).starts_with("**Writing Alloy that reads as Alloy**"));
        assert!(text(2).starts_with("**prefer_new**"), "{}", text(2));
        assert!(text(3).contains("\n## Articles\n"), "{}", text(3));
        assert!(text(3).contains("style"), "{}", text(3));

        for reply in &replies {
            assert_eq!(reply["result"]["isError"], false, "{reply}");
        }
    }

    /// The name outranks the body: a query that names a topic puts that
    /// topic first, and a word only an article holds finds the article.
    #[test]
    fn search_ranks_the_name_over_the_body() {
        let names = |query: &str| -> Vec<String> {
            search(query, 5).into_iter().map(|(name, _)| name).collect()
        };

        assert_eq!(names("match")[0], "match");
        assert_eq!(names("exhaustive")[0], "exhaustive");
        assert_eq!(names("style")[0], "style");
        assert!(names("yetanothernet").contains(&"style".to_string()));
        assert!(names("zzzz qqqq").is_empty());

        let hits = search("PowerTraining", 1);
        assert!(hits[0].1.contains("PowerTraining"), "{}", hits[0].1);
    }

    /// Every name the list gives opens a page, and that page holds the
    /// text the search read, so no two topics share a name.
    #[test]
    fn every_listed_topic_reads_back() {
        for t in topics() {
            let page = doc::markdown(&t.name).unwrap_or_else(|| panic!("no page for `{}`", t.name));

            assert!(
                page.contains(t.body.trim()),
                "`{}` opens another page",
                t.name
            );
        }
    }

    /// A cut never splits a character, wherever the match falls.
    #[test]
    fn a_snippet_cuts_on_a_character() {
        let body = "é".repeat(200);

        for at in 0..body.len() {
            let _ = snippet(&body, at);
        }
    }
}
