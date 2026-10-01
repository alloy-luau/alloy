//! Parallel Luau: the `parallel do ... end` block, the `message`
//! declaration and its calls, and the `.actor.` infix.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;

fn compile_as(file: &str, src: &str) -> alloy::Output {
    let options = alloy::EmitOptions {
        file_name: file.to_string(),
        ..alloy::EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

fn compile(src: &str) -> alloy::Output {
    compile_as("worker.server.actor.aly", src)
}

fn messages(out: &alloy::Output) -> Vec<String> {
    out.diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// The block keeps its lines and its scope: the two engine calls sit
/// on the lines of the header and of the `end`.
#[test]
fn a_parallel_block_desynchronizes_on_its_own_lines() {
    let src = "local hits = {}\nlocal parts: { BasePart } = {}\n\nparallel do\n    local hits = 0\n    for _, part in parts do\n        print(part.Position)\n    end\nend\nprint(hits)\n";
    let out = compile(src);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.ship.contains(
            "task.desynchronize() do\n    local hits = 0\n    for _, part in parts do\n        print(part.Position)\n    end\ntask.synchronize() end\nprint(hits)"
        ),
        "{}",
        out.ship
    );
    assert_eq!(out.ship.lines().count(), src.lines().count());
    assert_eq!(out.check.lines().count(), src.lines().count());
}

/// Each refusal names the rule in the words the RFC gives.
#[test]
fn the_parallel_phase_refuses_what_the_engine_refuses() {
    let src = "local part: BasePart = workspace:FindFirstChild(\"P\") :: BasePart\n\nlocal function paint(p: BasePart)\n    p.Color = Color3.new(1, 0, 0)\nend\n\nlocal function step()\n    parallel do\n        part.Position = Vector3.zero\n        part.Parent = nil\n        paint(part)\n        local copy = Instance.new(\"Part\")\n        copy:Destroy()\n        part:Clone()\n        destroy part\n        parallel do end\n        for i = 1, 3 do\n            if i > 1 then break end\n        end\n        local t = {}\n        t.x = 1\n        return\n    end\nend\n\nlocal async function wait_one()\n    parallel do\n        await Future.ready(1)\n    end\nend\n\nfor _ = 1, 2 do\n    parallel do\n        continue\n    end\nend\n\nstep()\nwait_one()\n";
    let got = messages(&compile(src));
    let want = [
        "a `parallel` block cannot write `part.Position`; move the write after the block",
        "a `parallel` block cannot write `part.Parent`; move the write after the block",
        "a `parallel` block cannot call `paint`, which writes `p.Color`; move the call after the block",
        "a `parallel` block cannot call `Instance.new`; move the call after the block",
        "a `parallel` block cannot call `copy:Destroy()`; move the call after the block",
        "a `parallel` block cannot call `part:Clone()`; move the call after the block",
        "a `parallel` block cannot destroy `part`; move the statement after the block",
        "a `parallel` block cannot hold a `parallel` block; the engine reads it as already desynchronized, so remove the inner `parallel do`",
        "a `parallel` block cannot `return`; the thread would leave in the parallel phase and skip `task.synchronize()`; set a local and return after the block",
        "a `parallel` block cannot `await`; a Future resumes in the serial phase, so the block would end where the source does not say; await after the block",
        "a `parallel` block cannot `continue` out of the block; the loop would go on in the parallel phase and skip `task.synchronize()`; set a local and `continue` after the block",
    ];

    for w in want {
        assert!(got.iter().any(|m| m == w), "missing {w:?} in {got:#?}");
    }

    // A `break` of a loop inside the block stays inside it, and a table
    // the file does not know as an Instance takes a write.
    assert!(
        !got.iter()
            .any(|m| m.contains("`break`") || m.contains("`t.x`")),
        "{got:#?}"
    );

    for m in &got {
        if m.starts_with("a `parallel` block") {
            assert_eq!(alloy::docs::kind_for(m), "ParallelError", "{m}");
        }
    }
}

/// A write through a value the file cannot type does not report: the
/// check is a guard rail, and the engine has the last word.
#[test]
fn a_write_through_an_unknown_value_stays_quiet() {
    let out = compile(
        "local function f(x)\n    parallel do\n        x.Position = 1\n    end\nend\nf(1)\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

/// `parallel` stays a name wherever it does not open a block.
#[test]
fn parallel_is_still_a_name() {
    let out = compile(
        "local parallel = true\nparallel = not parallel\nlocal t = { parallel = 1 }\nprint(parallel, t.parallel)\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.ship.contains("local parallel = true"), "{}", out.ship);
}

/// A message is the engine's own API with the topic filled in, and the
/// check artifact types the three calls from the parameters.
#[test]
fn a_message_writes_the_engine_calls() {
    let src = "message Step(dt: number, reply: Actor)\nmessage Hit(count: number) as parallel\n\nlocal worker = script.Parent :: Actor\n\nStep.fire(worker, 0.016, worker)\nStep.on(function(dt, reply)\n    Hit.fire(reply, 1)\nend)\nHit.on(function(count) print(count) end)\nHit.once(function(count)\n    print(count)\nend)\n";
    let out = compile(src);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    for line in [
        "local Step = { topic = \"Step\", fire = function(actor: Actor, ...) actor:SendMessage(\"Step\", ...) end }",
        "worker:SendMessage(\"Step\", 0.016, worker)",
        "script:GetActor():BindToMessage(\"Step\", function(dt, reply)",
        "    reply:SendMessage(\"Hit\", 1)",
        "script:GetActor():BindToMessageParallel(\"Hit\", function(count) print(count) end)",
        "__alloy.message_once(script:GetActor(), \"Hit\", true, function(count)",
    ] {
        assert!(out.ship.contains(line), "missing {line:?} in\n{}", out.ship);
    }

    assert!(
        out.check.contains("fire: (actor: Actor, dt: number, reply: Actor) -> (), on: (handler: (dt: number, reply: Actor) -> ()) -> RBXScriptConnection"),
        "{}",
        out.check
    );
    assert!(
        out.check.contains("Step.fire(worker, 0.016, worker)"),
        "{}",
        out.check
    );
    assert_eq!(out.ship.lines().count(), src.lines().count());
}

/// `on` binds on the Actor of the script, so a file with no `.actor.`
/// infix reports, and the fix names the file or the script to use.
#[test]
fn a_handler_needs_an_actor_script() {
    let src = "message Ping()\nPing.on(function() end)\n";
    let script = messages(&compile_as("src/main.server.aly", src));

    assert_eq!(
        script,
        vec![
            "`Ping.on` binds on the Actor of this script, and `main.server.aly` has no `.actor.` infix, so the build places it under no Actor; name the file `main.server.actor.aly`"
        ]
    );
    assert_eq!(alloy::docs::kind_for(&script[0]), "ActorError");

    let module = messages(&compile_as("src/net.aly", src));
    assert!(
        module[0].ends_with("a module sits under no Actor, so bind in a `.server.actor.aly` or `.client.actor.aly` script"),
        "{module:?}"
    );

    // `fire` reaches an Actor from any script.
    let fire = compile_as(
        "src/main.server.aly",
        "message Ping()\nPing.fire(script.Parent :: Actor)\n",
    );
    assert!(fire.diagnostics.is_empty(), "{:?}", fire.diagnostics);

    let bare = messages(&compile("message Ping()\nPing.fire()\n"));
    assert!(
        bare[0].starts_with("`Ping.fire` takes the Actor to send to first"),
        "{bare:?}"
    );
}

/// A parallel handler takes the rules of a `parallel` block, and a
/// serial one does not.
#[test]
fn a_parallel_handler_takes_the_block_rules() {
    let src = "message Hit(part: BasePart) as parallel\nmessage Calm(part: BasePart)\n\nlocal function paint(p: BasePart)\n    p.Color = Color3.new(1, 0, 0)\nend\n\nHit.on(function(part)\n    part.Parent = nil\n    return\nend)\nHit.once(paint)\nCalm.on(function(part)\n    part.Parent = nil\nend)\n";
    let got = messages(&compile(src));

    assert_eq!(
        got,
        vec![
            "the parallel handler of `Hit` cannot write `part.Parent`; call `task.synchronize()` before the write",
            "the parallel handler of `Hit` is `paint`, which writes `p.Color`; the handler runs in the parallel phase, so call `task.synchronize()` before the write",
        ]
    );
}

/// The engine passes the arguments as they are. What it cannot pass
/// whole reports as a wire type, with the rule named for actors.
#[test]
fn a_message_carries_only_data() {
    let src = "struct Shot as\n    x: number\nend\n\nmessage Bad(cb: () -> (), co: thread, shot: Shot, n: number = 2)\nmessage Ok(parts: { BasePart }, table: SharedTable, name: string?)\n";
    let got = messages(&compile(src));

    assert_eq!(
        got,
        vec![
            "message `Bad`: parameter `cb` has type `() -> ()`, which is a function type; an actor message carries only data",
            "message `Bad`: parameter `co` has type `thread`, which is a coroutine; an actor message carries only data",
            "message `Bad`: parameter `shot` has type `Shot`, whose methods live on a metatable that the engine drops on the way; an actor message carries only data",
            "parameter `n` of message `Bad` takes no default; the engine passes what `fire` sends, so pass the value there",
        ]
    );
    assert_eq!(alloy::docs::kind_for(&got[0]), "WireType");
    assert_eq!(alloy::docs::kind_for(&got[3]), "ActorError");
}

/// An attribute takes `message` as a target: the contract reads the
/// parameters as fields, and the value carries the attribute table.
#[test]
fn an_attribute_goes_on_a_message() {
    let src = "attribute replies on message as\n    requires field reply: Actor\nend\n\nattribute tick(rate: number) on message\n\n@replies\n@tick(30)\nexport message Step(dt: number, reply: Actor)\n\n@replies\nmessage Lost(dt: number)\n\n@tick(1)\nfunction f() end\n";
    let out = compile_as("src/net.aly", src);
    let got = messages(&out);

    assert_eq!(
        got,
        vec![
            "`@replies` requires a field `reply: Actor`; `Lost` declares none",
            "the attribute `tick` has no meaning on a function; it goes on `message`",
        ]
    );
    assert!(
        out.ship.contains(
            "local Step = __alloy.attrs({ topic = \"Step\", fire = function(actor: Actor, ...) actor:SendMessage(\"Step\", ...) end }, { own = { replies = {}, tick = { 30 } } })"
        ),
        "{}",
        out.ship
    );
}

/// `.actor.` goes on a script, after the side, and names the Actor
/// after the file.
#[test]
fn the_actor_infix_takes_a_script() {
    use alloy::directives::{actor_name_problem, file_side, is_actor};

    assert!(is_actor("src/physics.server.actor.aly"));
    assert!(is_actor("ui.client.actor.aly"));
    assert!(!is_actor("physics.server.aly"));
    assert!(!is_actor("init.server.actor.aly"));
    assert_eq!(
        file_side("physics.server.actor.aly"),
        Some(alloy::directives::Side::Server)
    );
    assert_eq!(actor_name_problem("src/physics.server.actor.aly"), None);
    assert_eq!(actor_name_problem("src/util.aly"), None);

    let module = actor_name_problem("src/util.actor.aly").unwrap();
    assert!(
        module.starts_with("`util.actor.aly` is a module"),
        "{module}"
    );
    assert!(module.ends_with("`util.server.actor.aly`"), "{module}");

    let order = actor_name_problem("x.actor.server.aly").unwrap();
    assert!(
        order.ends_with("name the file `x.server.actor.aly`"),
        "{order}"
    );

    let init = actor_name_problem("init.server.actor.aly").unwrap();
    assert!(
        init.contains("an `init` script takes the name of its folder"),
        "{init}"
    );

    // The compile reports the name at the top of the file.
    let out = compile_as("src/util.actor.aly", "return 1\n");
    assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
    assert_eq!(
        alloy::docs::kind_for(&out.diagnostics[0].message),
        "ActorError"
    );
}

/// An actor script sits one folder below its file in the game, so a
/// relative import climbs one more level.
#[test]
fn an_actor_script_climbs_one_more_folder() {
    let out = compile("import { f } from \"./util\"\nimport { g } from \"../shared/g\"\nf() g()\n");

    assert!(out.ship.contains("require(\"../util\")"), "{}", out.ship);
    assert!(
        out.ship.contains("require(\"../../shared/g\")"),
        "{}",
        out.ship
    );
    assert!(out.check.contains("require(\"../util\")"), "{}", out.check);
}

/// The quick fix moves a refused statement after the block when the
/// block reads nothing it writes, and keeps it in place otherwise.
#[test]
fn a_refused_write_moves_after_the_block() {
    let src = "local function step(part: BasePart)\n    parallel do\n        part.Position = Vector3.zero\n        print(part.Name)\n    end\nend\n";
    let at = src.find("part.Position").unwrap() as u32;
    let moved = alloy::desugar::parallel_move(src, at).expect("a move");
    let (a, b) = (moved.cut.0 as usize, moved.cut.1 as usize);
    let fixed = format!(
        "{}{}{}",
        &src[..a],
        &src[b..moved.insert_at as usize],
        moved.text
    ) + &src[moved.insert_at as usize..];

    assert_eq!(
        fixed,
        "local function step(part: BasePart)\n    parallel do\n        print(part.Name)\n    end\n    part.Position = Vector3.zero\nend\n"
    );
    assert!(compile(&fixed).diagnostics.is_empty());

    // Read later in the block: the write stays.
    let read = "local part: BasePart = nil :: any\nparallel do\n    part.Position = Vector3.zero\n    print(part.Position)\nend\n";
    assert!(
        alloy::desugar::parallel_move(read, read.find("part.Position").unwrap() as u32).is_none()
    );

    // A value the block declares ends with the block.
    let local = "local part: BasePart = nil :: any\nparallel do\n    local v = Vector3.zero\n    part.Position = v\nend\n";
    assert!(
        alloy::desugar::parallel_move(local, local.find("part.Position").unwrap() as u32).is_none()
    );

    // A write inside a loop runs more than once.
    let looped = "local part: BasePart = nil :: any\nparallel do\n    for i = 1, 2 do\n        part.Position = Vector3.zero\n    end\nend\n";
    assert!(
        alloy::desugar::parallel_move(looped, looped.find("part.Position").unwrap() as u32)
            .is_none()
    );
}

/// A reply rides a BindableEvent. The table of the message makes it,
/// `fire` sends it after the arguments, and the handler takes `respond`
/// in its place. A message with no reply keeps the plain table.
#[test]
fn a_reply_sends_the_event_and_gives_respond() {
    let src = "message Light(job: number, input: buffer) reply(job: number, levels: buffer)\nmessage Heat(job: number) reply() as parallel\nmessage Ping(n: number)\n\nlocal worker = script.Parent :: Actor\n\nLight.replied(function(job, levels) print(job, levels) end)\nLight.fire(worker, 7, buffer.create(1))\nLight.on(function(job, input, respond) respond(job, input) end)\nHeat.once(function(job, respond) respond() end)\nPing.fire(worker, 1)\n";
    let out = compile(src);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    for line in [
        "local Light = __alloy.message(\"Light\", 2)",
        "local Heat = __alloy.message(\"Heat\", 1)",
        "local Ping = { topic = \"Ping\", fire = function(actor: Actor, ...) actor:SendMessage(\"Ping\", ...) end }",
        "Light.replied(function(job, levels) print(job, levels) end)",
        "Light.fire(worker, 7, buffer.create(1))",
        "script:GetActor():BindToMessage(\"Light\", __alloy.message_respond(function(job, input, respond) respond(job, input) end, 2, false))",
        "__alloy.message_once(script:GetActor(), \"Heat\", true, __alloy.message_respond(function(job, respond) respond() end, 1, true))",
        "worker:SendMessage(\"Ping\", 1)",
    ] {
        assert!(out.ship.contains(line), "missing {line:?} in\n{}", out.ship);
    }

    for part in [
        "on: (handler: (job: number, input: buffer, respond: (job: number, levels: buffer) -> ()) -> ()) -> RBXScriptConnection",
        "replied: (handler: (job: number, levels: buffer) -> ()) -> RBXScriptConnection",
        "on: (handler: (job: number, respond: () -> ()) -> ()) -> RBXScriptConnection",
        "Light.on(function(job, input, respond) respond(job, input) end)",
    ] {
        assert!(
            out.check.contains(part),
            "missing {part:?} in\n{}",
            out.check
        );
    }

    assert!(!out.check.contains("Ping = (__alloy"), "{}", out.check);
    assert_eq!(out.ship.lines().count(), src.lines().count());
    assert_eq!(out.check.lines().count(), src.lines().count());
}

/// `replied` binds on any script, since the answer comes back on an
/// event and not at an Actor.
#[test]
fn replied_binds_on_any_script() {
    let out = compile_as(
        "src/main.server.aly",
        "message Light(job: number) reply(job: number)\nLight.replied(function(job) print(job) end)\nLight.fire(script.Parent :: Actor, 1)\n",
    );

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

/// Each misuse of a reply names the message and says what to write.
#[test]
fn a_reply_reports_its_misuse() {
    let src = "struct Shot as\n    x: number\nend\n\nmessage Light(job: number) reply(job: number, levels: buffer)\nmessage Ping(n: number)\nmessage Many(...: number) reply(n: number)\nmessage Bad(n: number) reply(cb: () -> (), shot: Shot, k: number = 1)\n\nPing.replied(function() end)\nLight.replied(print, print)\nLight.replied(function(job, levels, extra) end)\nPing.on(function(n, respond) end)\nLight.on(function(job, respond, extra)\n    respond(job)\n    respond(job, buffer.create(1))\n    respond(job, table.unpack({}))\nend)\n";
    let got = messages(&compile(src));
    let want = [
        "message `Many` ends its parameters with `...`, so it takes no `reply`: the handler takes `respond` after the parameters, and `...` has no end; name each parameter",
        "message `Bad`: reply parameter `cb` has type `() -> ()`, which is a function type; an actor message carries only data",
        "message `Bad`: reply parameter `shot` has type `Shot`, whose methods live on a metatable that the engine drops on the way; an actor message carries only data",
        "reply parameter `k` of message `Bad` takes no default; the event passes what `respond` sends, so pass the value there",
        "message `Ping` declares no `reply`, so `Ping.replied` has no answer to bind; write `reply(...)` after its parameters",
        "`Light.replied` takes one handler of the reply of message `Light`: `Light.replied(function(job, levels) end)`",
        "the handler of `Light.replied` takes the reply of message `Light`, `(job: number, levels: buffer)`; `extra` has nothing behind it",
        "the handler of `Ping.on` names `respond` after the parameters of message `Ping`, and the message declares no `reply`, so `respond` has nothing behind it; write `reply(...)` after the parameters to answer",
        "the handler of `Light.on` takes the parameters of message `Light` and then `respond`; `extra` has nothing behind it",
        "`respond` answers message `Light` with its reply, `(job: number, levels: buffer)`, which takes 2 values; this call passes 1",
    ];

    assert_eq!(got, want);

    for m in &got {
        let kind = alloy::docs::kind_for(m);
        let wire = m.ends_with("an actor message carries only data");
        assert_eq!(kind, if wire { "WireType" } else { "ActorError" }, "{m}");
    }
}

/// A call of `respond` may leave out the optional values at the end of
/// the reply, and passes no more than the reply holds.
#[test]
fn respond_may_leave_out_optional_values() {
    let src = "message Note(job: number) reply(job: number, text: string?, more: number?)\nNote.on(function(job, respond)\n    respond(job)\n    respond(job, \"a\", 1)\n    respond()\n    respond(job, nil, 1, 2)\nend)\n";
    let got = messages(&compile(src));

    assert_eq!(
        got,
        vec![
            "`respond` answers message `Note` with its reply, `(job: number, text: string?, more: number?)`, which takes 1 to 3 values; this call passes 0",
            "`respond` answers message `Note` with its reply, `(job: number, text: string?, more: number?)`, which takes 1 to 3 values; this call passes 4",
        ]
    );
}

/// `respond` synchronizes before it fires, so a parallel handler runs
/// serial after a `respond` or a `task.synchronize()` that its body
/// holds directly. One inside a branch may not run, so the check goes on.
#[test]
fn respond_ends_the_parallel_phase() {
    let src = "message Hit(part: BasePart) reply(ok: boolean) as parallel\n\nHit.on(function(part, respond)\n    part.Parent = nil\n    respond(true)\n    part.Name = \"hit\"\nend)\nHit.once(function(part, respond)\n    task.synchronize()\n    part.Name = \"once\"\nend)\nHit.on(function(part, done)\n    if part.Anchored then\n        done(false)\n    end\n    part.Name = \"late\"\nend)\n";
    let got = messages(&compile(src));

    assert_eq!(
        got,
        vec![
            "the parallel handler of `Hit` cannot write `part.Parent`; call `task.synchronize()` before the write",
            "the parallel handler of `Hit` cannot write `part.Name`; call `task.synchronize()` before the write",
        ]
    );
    // The second report is the write after the branch.
    let at = compile(src).diagnostics[1].start as usize;
    let line = src[at..].lines().next().unwrap_or_default();
    assert_eq!(line, "part.Name = \"late\"");
}

/// An imported message carries its reply: the handler takes `respond`,
/// and `fire` goes through the table.
#[test]
fn an_imported_reply_reads_the_declaration() {
    let options = alloy::EmitOptions {
        file_name: "worker.server.actor.aly".to_string(),
        import_messages: vec![
            (
                "Light".to_string(),
                alloy::desugar::MessageSig {
                    topic: "Light".to_string(),
                    params: 1,
                    reply: Some(vec!["levels: buffer".to_string()]),
                    ..Default::default()
                },
            ),
            (
                "Ping".to_string(),
                alloy::desugar::MessageSig {
                    topic: "Ping".to_string(),
                    params: 1,
                    ..Default::default()
                },
            ),
        ],
        ..alloy::EmitOptions::default()
    };
    let src = "import { Light, Ping } from \"./net\"\nLight.on(function(job, respond) respond(buffer.create(1)) end)\nLight.fire(script.Parent :: Actor, 1)\nPing.replied(print)\n";
    let out = alloy::compile_with(src, &options).unwrap();

    assert_eq!(
        messages(&out),
        vec![
            "message `Ping` declares no `reply`, so `Ping.replied` has no answer to bind; write `reply(...)` after its parameters"
        ]
    );
    assert!(
        out.ship.contains("script:GetActor():BindToMessage(\"Light\", __alloy.message_respond(function(job, respond) respond(buffer.create(1)) end, 1, false))"),
        "{}",
        out.ship
    );
    assert!(!out.ship.contains("SendMessage(\"Light\""), "{}", out.ship);
}

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-parallel-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/server")).unwrap();
    fs::create_dir_all(dir.join("src/shared")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"p\"\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n",
    )
    .unwrap();

    dir
}

fn build(root: &Path) -> alloy::build::Report {
    let config = Config::load(&root.join("alloy.toml")).unwrap();

    alloy::build::run_project(root, &config).unwrap()
}

/// The build writes the Actor: the script in a folder of its name with
/// the meta file that gives the folder its class. The sourcemap holds
/// the Actor, and an imported message writes the engine call.
#[test]
fn the_build_writes_an_actor_with_the_script_inside() {
    let dir = project("build");
    fs::write(
        dir.join("src/shared/net.aly"),
        "export message Step(dt: number) as parallel\n",
    )
    .unwrap();
    fs::write(dir.join("src/server/util.aly"), "export function f() end\n").unwrap();
    fs::write(
        dir.join("src/server/worker.server.actor.aly"),
        "import { Step as Tick } from \"@shared/net\"\nimport { f } from \"./util\"\n\nTick.on(function(dt)\n    f()\nend)\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let script = fs::read_to_string(dir.join("build/server/worker/worker.server.luau")).unwrap();
    assert!(
        script.contains("script:GetActor():BindToMessageParallel(\"Step\", function(dt)"),
        "{script}"
    );
    assert!(script.contains("require(\"../util\")"), "{script}");
    assert_eq!(
        fs::read_to_string(dir.join("build/server/worker/init.meta.json")).unwrap(),
        alloy::project::ACTOR_META
    );
    assert!(!dir.join("build/server/worker.server.actor.luau").exists());

    let map: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("sourcemap.json")).unwrap()).unwrap();
    let server = &map["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ServerScriptService")
        .unwrap()["children"][0]["children"];
    let actor = server
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "worker")
        .unwrap();
    assert_eq!(actor["className"], "Actor");
    assert_eq!(actor["children"][0]["className"], "Script");
    assert_eq!(
        actor["children"][0]["filePaths"][0],
        "src/server/worker.server.actor.aly"
    );

    // A rename back to a plain script takes the Actor folder away.
    fs::rename(
        dir.join("src/server/worker.server.actor.aly"),
        dir.join("src/server/worker.server.aly"),
    )
    .unwrap();
    fs::write(dir.join("src/server/worker.server.aly"), "print(1)\n").unwrap();
    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", report.diagnostics);
    assert!(!dir.join("build/server/worker").exists());
    assert!(dir.join("build/server/worker.server.luau").is_file());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn actor_output_moves_the_script_into_its_actor() {
    assert_eq!(
        alloy::project::actor_output(Path::new("server/physics.server.actor.luau")),
        Some((
            PathBuf::from("server/physics/physics.server.luau"),
            PathBuf::from("server/physics/init.meta.json")
        ))
    );
    assert_eq!(
        alloy::project::actor_output(Path::new("physics.server.luau")),
        None
    );
}

/// The type errors of `alloy flux` on a project, as `file:line:col message`.
/// `None` when luau-lsp is not installed.
fn flux_errors(root: &Path) -> Option<Vec<String>> {
    let config = Config::load(&root.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(root, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);
    alloy::typecheck::find_luau_lsp(&config.flux)?;

    let analysis = alloy::typecheck::analyze(root, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");

    Some(
        analysis
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| format!("{}:{}:{} {}", d.rel.display(), d.line, d.col, d.message))
            .collect(),
    )
}

/*
An actor script ships inside its Actor, so its relative imports climb
one more folder. `alloy flux` put its check artifact at the file's own
place, and with no sourcemap to place the Actor, luau-lsp read `./jobs`
one folder too high: a project with no Rojo file reported UnknownModule.
The artifact now sits in the folder of its Actor, with the meta file, in
a project with no tree, with a tree of mounts, and with a Rojo file. The
one report left in each is the wrong type the test writes.
*/
#[test]
fn an_actor_script_imports_a_sibling_in_every_layout() {
    let layouts = [
        ("none", "[build]\nin = \"src\"\nout = \"build\"\n", None),
        (
            "mounts",
            "[build]\nin = \"src\"\nout = \"build\"\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\n",
            None,
        ),
        (
            "rojo",
            "[build]\nin = \"src\"\nout = \"build\"\n",
            Some(
                "{\n  \"name\": \"p\",\n  \"tree\": {\n    \"$className\": \"DataModel\",\n    \"ServerScriptService\": { \"Server\": { \"$path\": \"build/server\" } }\n  }\n}\n",
            ),
        ),
    ];

    for (name, toml, rojo) in layouts {
        let dir =
            std::env::temp_dir().join(format!("alloy-actor-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/server")).unwrap();
        fs::write(dir.join("alloy.toml"), toml).unwrap();

        if let Some(project) = rojo {
            fs::write(dir.join("default.project.json"), project).unwrap();
        }

        fs::write(
            dir.join("src/server/jobs.aly"),
            "--- A job.\nexport message Light(job: number)\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/server/worker.server.actor.aly"),
            "import { Light } from './jobs'\n\nLight.on(function(job)\n    const n: string = job\n    print(n)\nend)\n",
        )
        .unwrap();

        let Some(errors) = flux_errors(&dir) else {
            eprintln!("skipped: luau-lsp is not installed");

            return;
        };

        assert_eq!(
            errors,
            vec!["server/worker.server.actor.aly:4:23 Expected this to be 'string', but got 'number'".to_string()],
            "{name}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
