local __alloy = require("@alloy") --- Writes a line to the output.
local function log(msg: string)
    print(msg)
end

local counter = 0

const MAX = 10

export type Id = number

local Vec2 = {} Vec2.__index = Vec2 setmetatable(Vec2, { __call = function(_, f)  return setmetatable(f, Vec2) end }) function Vec2.new(f) return Vec2(f) end


 export type Vec2 = typeof(setmetatable({} :: { x: number, y: number }, Vec2)) Vec2.__tostring = function(s) return __alloy.show_struct("Vec2", s, { "x", "y" }) end

local State = {} State.__index = State
    State.Idle = "Idle"
    State.Busy = "Busy"
function State.is(v) return (type(v) == "table" and getmetatable(v) == State) or v == "Idle" or v == "Busy" end State.__tostring = function(v) return __alloy.show_variant("State", v) end export type State = "Idle" | "Busy"

export type Named = { name: string }



log(`{counter} {MAX}`) return { log = log, counter = counter, MAX = MAX, Vec2 = Vec2, State = State }
