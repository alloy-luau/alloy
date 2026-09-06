local __alloy = require("@alloy") -- Enums carry payloads. `match` compiles to an if-chain on the tag.
-- Every Alloy block closes with `end`.

-- A payload enum desugars to tagged tables.
local Msg = {} Msg.__index = Msg
    function Msg.Join(_1) return setmetatable({ tag = "Join", _1 = _1 }, Msg) end
    function Msg.Chat(_1, _2) return setmetatable({ tag = "Chat", _1 = _1, _2 = _2 }, Msg) end
    function Msg.Leave(_1) return setmetatable({ tag = "Leave", _1 = _1 }, Msg) end
function Msg.is(v) return (type(v) == "table" and getmetatable(v) == Msg) end Msg.__tostring = function(v) return __alloy.show_variant("Msg", v) end type Msg = { tag: "Join", _1: Player } | { tag: "Chat", _1: Player, _2: string } | { tag: "Leave", _1: Player }

-- A unit enum emits a string literal union plus a table.
-- Color.Red == "Red" at runtime, so it serializes for free.
-- Members separate by newline, `,`, or `;`, so one line works too.
local Color = {} Color.__index = Color Color.Red = "Red"  Color.Green = "Green"  Color.Blue = "Blue" function Color.is(v) return (type(v) == "table" and getmetatable(v) == Color) or v == "Red" or v == "Green" or v == "Blue" end Color.__tostring = function(v) return __alloy.show_variant("Color", v) end type Color = "Red" | "Green" | "Blue"

local Rarity = {} Rarity.__index = Rarity
    Rarity.Common = "Common";
    Rarity.Rare = "Rare";
    Rarity.Legendary = "Legendary"
function Rarity.is(v) return (type(v) == "table" and getmetatable(v) == Rarity) or v == "Common" or v == "Rare" or v == "Legendary" end Rarity.__tostring = function(v) return __alloy.show_variant("Rarity", v) end type Rarity = "Common" | "Rare" | "Legendary"

-- `impl` on an enum adds methods, as on a struct.
do
    function Msg.player(self): Player
        return (
            if type(self) == "table" and self.tag == "Join" then self._1
            elseif type(self) == "table" and self.tag == "Chat" then self._1
            else self._1
        )
    end
end

-- Match is an expression. Exhaustiveness is checked from the enum's
-- variant list, so no `default` is needed here. A missing arm is an error.
local function describe(msg: Msg): string
    return (
        if type(msg) == "table" and msg.tag == "Join" then `{msg._1.Name} joined`
        -- A guard is `and <expr>` after the pattern.
        elseif type(msg) == "table" and msg.tag == "Chat" and (#msg._2 > 200) then `{msg._1.Name} sent a wall of text`
        elseif type(msg) == "table" and msg.tag == "Chat" then `{msg._1.Name}: {msg._2}`
        else `{msg._1.Name} left`
    )
end

-- Match as a statement. Each arm holds a block that runs to the next
-- `case`, `default`, or `end`. A normal `if` inside an arm stays readable.
local function handle(msg: Msg)
    do local _m1 = msg
        if type(_m1) == "table" and _m1.tag == "Join" then local p = _m1._1
            if p.AccountAge < 7 then print("new player") end
            print("welcome", p.Name)
            p:LoadCharacter()
        elseif type(_m1) == "table" and _m1.tag == "Chat" then local p, text = _m1._1, _m1._2
            -- An arm is its own scope and may return.
            local line = `{p.Name}: {text}`
            print(line)
            return
        else
            -- nothing
    end end
end

-- No parentheses around the scrutinee: `match(x)` is a call to a function
-- named match, as in plain Luau.
local function is_join(msg: Msg): boolean
    return (
        if type(msg) == "table" and msg.tag == "Join" then true
        else false
    )
end

-- A literal or string scrutinee needs a `default` arm.
local function hex(color: Color): string
    return (
        if color == "Red" then "#ff0000"
        elseif color == "Green" then "#00ff00"
        elseif color == "Blue" then "#0000ff"
        else "#000000"
    )
end

-- Two scrutinees, from upstream's proposal.
local function leap(year: number): boolean
    local _1 = year % 400 local _2 = year % 100 local _3 = year % 4 return (
        if _1 == 0 then true
        elseif _2 == 0 then false
        elseif _3 == 0 then true
        else false
    )
end

-- Result is an ordinary library enum.
local parsed: __alloy.Result<number, string> = __alloy.Ok(1)
do local _m1 = parsed
    if type(_m1) == "table" and _m1.tag == "Ok" then local n = _m1._1 print(n)
    elseif type(_m1) == "table" and _m1.tag == "Err" then local e = _m1._1 warn(e)
end end

print(describe, handle, is_join, hex(Color.Red), leap(2024), Msg.Join)
