--- An enum with no `as`, whose variants have doc comments.
local __alloy = require("@alloy") local Kind = {} Kind.__index = Kind
  --- The first variant, with a comma, here.
  Kind.Alpha = ("Alpha" :: Kind)
  --- The second variant, also, with commas.
  function Kind.Beta(_1: number): typeof(setmetatable(({} :: any) :: { tag: "Beta", _1: number }, Kind)) return (setmetatable({ tag = "Beta", _1 = _1 }, Kind) :: any) end
  --- The third variant.
  Kind.Gamma = ("Gamma" :: Kind)
function Kind.is(v) return (type(v) == "table" and getmetatable(v) == Kind) or v == "Alpha" or v == "Gamma" end Kind.__tostring = function(v) return __alloy.show_variant("Kind", v) end export type Kind = "Alpha" | typeof(setmetatable(({} :: any) :: { tag: "Beta", _1: number }, Kind)) | "Gamma"

local Pick = {} Pick.__index = Pick
  --- Left, the default.
  Pick.Left = ("Left" :: Pick)  -- after the comma, still a comment
  Pick.Right = ("Right" :: Pick)
function Pick.is(v) return (type(v) == "table" and getmetatable(v) == Pick) or v == "Left" or v == "Right" end Pick.__tostring = function(v) return __alloy.show_variant("Pick", v) end export type Pick = "Left" | "Right" return { Kind = Kind, Pick = Pick }
