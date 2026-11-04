--!strict
--[[
  A module header. The runtime require goes on the blank line below
  it, so the doc comment stays directly above its statement.
]]
local __alloy = require("@alloy") 
--- The doc comment of the first statement.
export type Id = number

--- A struct, so the module needs the runtime.
local Tag = {} Tag.__index = Tag export type Tag = typeof(setmetatable(({} :: any) :: {
id: Id,
 }, Tag)) setmetatable(Tag, { __call = function(_, f: { id: Id }): Tag  return ((setmetatable(f, Tag) :: any) :: Tag) end }) function Tag.new(f: { id: Id }): Tag return Tag(f) end Tag.__tostring = function(s) return __alloy.show_struct("Tag", s, { "id" }) end return { Tag = Tag }
