**Writing Alloy that reads as Alloy**

Alloy code must read as Alloy, not as Luau with Alloy syntax. Each rule below is one instruction. Where an example helps, the `before` block shows the old form, and the `after` block shows the form to write. The examples follow the lints, so a name that never changes is a `const`.

`alloy fmt` sets the layout, and `alloy flux` reports what a lint can see. `alloy doc lints` lists the lints of the `style` group.

**File kinds**

Alloy has three kinds of source file:

- `x.aly` is a module. The build writes it as `x.luau`.
- `x.alx` is a module with markup in it: tags, attributes, and text, the way JSX sits in TypeScript. The build writes it as `x.luau` too. `alloy doc markup` explains the markup.
- `x.d.aly` holds declarations. The build writes no output for it. Every `.d.aly` under `[build] in` that `exclude` keeps joins the type check of the editor and of `alloy flux`.

**Syntax**

Construct with `new`. Write `new Vector3(x, y, z)`, never `Vector3.new(x, y, z)`. The `prefer_new` lint reports the old form, and `alloy flux --fix` rewrites it.

```luau
-- before
local origin = Vector3.new(0, 10, 0)
local part = Instance.new("Part")
```

```alloy
-- after
const origin = new Vector3(0, 10, 0)
const part = new Instance("Part") { Position = origin }
```

Use `?.` and `??` for a value that can be nil.

```luau
-- before
local function volume(settings: { volume: number? }?): number
    if settings and settings.volume ~= nil then
        return settings.volume
    end
    return 0.5
end
```

```alloy
-- after
local function volume(settings: { volume: number? }?) -> number
    return settings?.volume ?? 0.5
end
```

Use `is` for a type test.

```luau
-- before
local function size_of(value: unknown): number
    if typeof(value) == "Vector3" then
        return value.Magnitude
    end
    return 0
end
```

```alloy
-- after
local function size_of(value: unknown) -> number
    if value is Vector3 then
        return value.Magnitude
    end
    return 0
end
```

Bind and test a value in one step with `if local`. Write `if const` when the name never changes; the `prefer_const` lint asks for it.

```luau
-- before
local function place(player: Player, spawn: CFrame)
    local character = player.Character
    if character then
        character:PivotTo(spawn)
    end
end
```

```alloy
-- after
local function place(player: Player, spawn: CFrame) -> ()
    if const character = player.Character then
        character:PivotTo(spawn)
    end
end
```

Use `->` for a child lookup. `a->b` is `a:FindFirstChild("b")`, and it is nil when the child is missing.

```luau
-- before
local spawns = workspace:FindFirstChild("Map") and workspace.Map:FindFirstChild("Spawns")
```

```alloy
-- after
const spawns = workspace->Map->Spawns
```

Use the ternary `c ? a : b` where it reads more clearly than `if c then a else b`.

```luau
-- before
local function label(count: number): string
    return if count == 1 then "item" else "items"
end
```

```alloy
-- after
local function label(count: number) -> string
    return count == 1 ? "item" : "items"
end
```

Write an empty return type as `-> ()`.

```luau
-- before
local function reset(player: Player): ()
    player:LoadCharacter()
end
```

```alloy
-- after
local function reset(player: Player) -> ()
    player:LoadCharacter()
end
```

Use `@cfg(client)` and `@cfg(server)`, not `RunService:IsClient()` checks. A function under `@cfg` that returns no value returns at once on the other side.

```luau
-- before
local RunService = game:GetService("RunService")

local function start_client()
    print("the client starts")
end

if RunService:IsClient() then
    start_client()
end
```

```alloy
-- after
@cfg(client)
local function start_client() -> ()
    print("the client starts")
end

start_client()
```

Use the std `HashMap` and `Set` for keyed collections.

```luau
-- before
local coins: { [Player]: number } = {}
local seen: { [string]: boolean } = {}
```

```alloy
-- after
import { HashMap, Set } from "@alloy/std/collections"

const coins: HashMap<Player, number> = new HashMap()
const seen: Set<string> = new Set()
```

Use a namespace to group related code. Outside, a member reads as `Name.member`.

```alloy
namespace Shop
    const TAX = 0.1

    function price(base: number) -> number
        return base * (1 + TAX)
    end
end

print(Shop.price(10))
```

**Forms behind a flag**

Some forms are not final. Each one is behind a flag under `[fflags]` in `alloy.toml`, and the doc page of each form names its flag: `alloy doc '|>'`, `alloy doc comprehension`. Read the `[fflags]` table of the project before you write code. Where a flag is on, write its form: a project that turns a flag on wants the form in its code. Where a flag is off, the compiler reports the form, so write the older one. `OnByDefault = true` in the table turns on each experimental flag that has no entry of its own, so read it as every flag on.

Use the pipe `|>` for a value that goes through a chain of calls. `_` reads the value of the stage before. The flag is `AlloyExperimentalPipeOperator`.

```luau
-- before
local function label(humanoid: Humanoid): string
    local health = math.clamp(humanoid.Health, 0, humanoid.MaxHealth)
    return string.format("%d / %d", math.floor(health), humanoid.MaxHealth)
end
```

```alloy
-- after
local function label(humanoid: Humanoid) -> string
    return humanoid.Health
        |> math.clamp(_, 0, humanoid.MaxHealth)
        |> math.floor(_)
        |> string.format("%d / %d", _, humanoid.MaxHealth)
end
```

Use the append `:=` to add an item to a list, a `Set`, or a `Queue`. The flag is `AlloyExperimentalWalrusSyntax`.

```luau
-- before
local names = { "ada" }
table.insert(names, "grace")
```

```alloy
-- after
const names = { "ada" }
names := "grace"
```

Use the table library and the string methods as methods. The flags are `AlloyExperimentalTableStdInheritance` and `AlloyExperimentalStringMethods`.

```luau
-- before
local function command(input: string, history: { string }): string?
    local name = string.match(input, "^%s*(.-)%s*$")
    if string.sub(name, 1, 1) ~= "/" then
        return nil
    end
    table.insert(history, name)
    return name
end
```

```alloy
-- after
local function command(input: string, history: { string }) -> string?
    const name = input:trim()
    if not name:starts_with("/") then
        return nil
    end
    history:insert(name)
    return name
end
```

Use `??=` for a default on nil. Use `or=` for a default on nil or false, and `and=` for the opposite. The flag of `or=` and `and=` is `AlloyExperimentalLogicalAssignSyntax`; `??=` needs no flag.

```luau
-- before
local function setup(options: { retries: number?, label: string? })
    if options.retries == nil then
        options.retries = 3
    end
    options.label = options.label or "none"
end
```

```alloy
-- after
local function setup(options: { retries: number?, label: string? }) -> ()
    options.retries ??= 3
    options.label or= "none"
end
```

Use a chained comparison for a range test. The flag is `AlloyExperimentalChainedComparisonSyntax`.

```luau
-- before
local function pick(items: { string }, index: number): string?
    if 1 <= index and index <= #items then
        return items[index]
    end
    return nil
end
```

```alloy
-- after
local function pick(items: { string }, index: number) -> string?
    if 1 <= index <= #items then
        return items[index]
    end
    return nil
end
```

Use a comprehension for a list that one loop builds. The flag is `AlloyExperimentalComprehensionSyntax`.

```luau
-- before
local scores = { 3, 8, 5 }
local high = {}
for _, score in scores do
    if score > 4 then
        table.insert(high, score * 2)
    end
end
print(high)
```

```alloy
-- after
const scores = { 3, 8, 5 }
const high = [ score * 2 for _, score in scores if score > 4 ]
print(high)
```

Use a generator for a sequence that a loop reads one value at a time. The flag is `AlloyExperimentalGeneratorSyntax`.

```alloy
function* countdown(from: number)
    for i = from, 1, -1 do
        yield i
    end
end

for n in countdown(3) do
    print(n)
end
```

Use a lambda for a callback of one expression. The flag is `AlloyExperimentalLambdaSyntax`. A callback with statements keeps `function`.

```luau
-- before
table.sort(players, function(a, b)
    return a.score > b.score
end)
```

```alloy
-- after
local players = { { score = 2 }, { score = 5 } }
table.sort(players, |a, b| a.score > b.score)
```

Use `defer` for the cleanup of a function with more than one exit. The flag is `AlloyExperimentalDeferSyntax`. The block runs at each `return` and at an error.

```luau
-- before
local function probe(part: BasePart): boolean
    local touch = part.Touched:Connect(print)
    if part.Anchored then
        touch:Disconnect()
        return false
    end
    task.wait(1)
    touch:Disconnect()
    return true
end
```

```alloy
-- after
local function probe(part: BasePart) -> boolean
    const touch = part.Touched:Connect(print)
    defer touch:Disconnect()

    if part.Anchored then
        return false
    end

    task.wait(1)
    return true
end
```

Use `using` for a value that the function must drop when it ends: a connection, an Instance, a lock. The flag is `AlloyExperimentalUsingSyntax`. The drop runs at each `return` and at an error, in one order with each `defer`.

```luau
-- before
local function flash(part: BasePart)
    local marker = Instance.new("Highlight")
    marker.Parent = part
    task.wait(1)
    marker:Destroy()
end
```

```alloy
-- after
local function flash(part: BasePart) -> ()
    using marker = new Instance("Highlight") { Parent = part }
    task.wait(1)
end
```

Leave on nil with a `return`, a `break`, or a `continue` after `??`. The flag is `AlloyExperimentalBailSyntax`. The name is not nil on the lines after it.

```luau
-- before
local function title(player: Player): string
    local character = player.Character
    if character == nil then
        return ""
    end
    return character.Name
end
```

```alloy
-- after
local function title(player: Player) -> string
    const character = player.Character ?? return ""
    return character.Name
end
```

Use a range arm in a `match` on a number. `1..10` stops before 10, and `1..=10` takes 10 too. The flag is `AlloyExperimentalRangePatternSyntax`.

```luau
-- before
local function rank(level: number): string
    if level >= 1 and level < 10 then
        return "bronze"
    elseif level >= 10 and level <= 49 then
        return "silver"
    end
    return "unranked"
end
```

```alloy
-- after
local function rank(level: number) -> string
    match level with
        case 1..10 then return "bronze"
        case 10..=49 then return "silver"
        default return "unranked"
    end
end
```

Use a newtype for a number or a string that must not mix with another one: an id, an amount, a key. `new` wraps the value, and `raw` reads it. The flag is `AlloyExperimentalNewtypeSyntax`.

```luau
-- before
local function pay(user_id: number, coins: number)
    print(user_id, coins)
end

pay(5, 42)
```

```alloy
-- after
struct UserId(number)
struct Coins(number)

local function pay(id: UserId, amount: Coins) -> ()
    print(UserId.raw(id), Coins.raw(amount))
end

pay(new UserId(42), new Coins(5))
```

Put `@must_use` on a function whose result the caller must read: a success flag, a `Result`, a handle. The `unused_must_use` lint reports a call that drops the result. The flag is `AlloyExperimentalMustUse`.

```alloy
@must_use("false means the player had too few coins")
local function spend(coins: number, cost: number) -> boolean
    return coins >= cost
end

if not spend(3, 5) then
    print("too few coins")
end
```

Use `import inline` for a small `.json` or `.toml` file that one module reads. The table goes into the output of that module, and the file needs no mount. The flag is `AlloyExperimentalInlineDataSyntax`. `alloy doc inline-data` has the rules.

```
import inline limits from "../config/limits.toml"
import inline { coins, pets } from "../config/game.json"
```

Use a width type on a field or a remote parameter that crosses the network. The name replaces the attribute and the `number`. The flag is `AlloyExperimentalWidthTypes`.

```luau
-- before
struct Shot
    @f32 x: number
    @u8 power: number
end
```

```alloy
-- after
struct Shot
    x: f32
    power: u8
end
```

**Std**

`alloy doc std` lists the types of the std, and `alloy doc Array` lists the methods of one type. Look a method up before you write a loop that does the same work.

Use the Array methods for the work of a loop over a list: `any`, `all`, `sum`, `min_by`, `max_by`, `flat_map`, `flatten`, `group_by`, `chunk`, `zip`, `unique`, `take`, `skip`, `insert_at`, and `binary_search`.

```luau
-- before
local scores = { 3, 8, 5 }
local total = 0
local high = false
for _, score in scores do
    total += score
    if score > 7 then
        high = true
    end
end
print(total, high)
```

```alloy
-- after
const scores = [ 3, 8, 5 ]
print(scores:sum(), scores:any(|score| score > 7))
```

Use `map_or` and `flatten` on a `Result`, not a `match` that only unwraps.

```alloy
local function half(n: number) -> Result<number, string>
    return n % 2 == 0 ? Ok(n / 2) : Err("odd")
end

print(half(8):map_or(0, |n| n + 1))
```

Some functions of the std are behind `AlloyExperimentalStdAdditions`: `Array.range`, `Result.all`, `Future.timeout`, `Future.retry`, `Future.from_signal`, `Future.debounce`, and `Future.throttle`. The methods above need no flag.

```alloy
local function load() -> number
    return 1
end

const save = Future.debounce(|text: string| print(text), 0.5)
save("draft")

async do
    const value = await Future.timeout(Future.retry(load, 3, 0.5), 10)
    print(value, Array.range(1, 5):sum())
end
```

Use `Net` for an HTTP request, not `HttpService:RequestAsync`. A request gives a `Future` of a `Result`. The flag is `AlloyExperimentalNetLibrary`.

```alloy
import { Net } from "@alloy/std/net"

async do
    match await Net.get("https://example.com/items"):timeout(5):retries(3, 0.25):send() with
        case Ok(response) then print(response.status, response.body)
        case Err(e) then print(e.kind, e.message)
    end
end
```

**Networking**

Use the `remote` keyword or yetanothernet routes. Do not call `Instance.new("RemoteEvent")`. A `remote` is a typed channel between the server and the client.

```luau
-- before
local ReplicatedStorage = game:GetService("ReplicatedStorage")

local damage = Instance.new("RemoteEvent")
damage.Name = "Damage"
damage.Parent = ReplicatedStorage
```

```alloy
-- after
--- A client asks the server to damage a player.
export remote Damage(target: Player, amount: number) from client
```

`alloy doc remote` explains the declaration and the methods it gives.

**Layout**

Sort the files into folders the way PowerTraining does: `shared/types`, `shared/data`, `server/Providers`, and `systems/`.

```
src/
  client/
    main.client.aly
    systems/
  server/
    main.server.aly
    Providers/
    systems/
  shared/
    startup.aly
    components.aly
    data/
    types/
```

Start the game from `src/shared/startup.aly`. Keep the entry scripts short: an entry script imports the startup and calls it.

```alloy
import { startup } from "@shared/startup"

startup()
```

Write a system as a named function. Put the export at the bottom of the file.

Keep all components in one table, and import it as `ct`.

```alloy
import ct from "@shared/components"
import { world } from "@shared/world"

--- Heals each living entity by one point a frame.
local function regen() -> ()
    for entity, health in world:query(ct.Health) do
        world:set(entity, ct.Health, health + 1)
    end
end

export default { name = "regen", system = regen }
```

Keep real constants in a constants file. Give each constant a doc comment, and put a blank line between two documented constants. Put a long explanation in a `--[[ ]]` block.

```alloy
--[[
    The server's tuning: how long it waits, and how often it saves.
    A long explanation of a constant goes in a block like this one.
]]

--- Seconds between two saves of a player's data.
export const AUTOSAVE_SECONDS = 60

--- Seconds from a death to the next spawn.
export const RESPAWN_SECONDS = 3
```

**Docs**

Every game ships docs and a maintainers guide.
