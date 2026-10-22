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
