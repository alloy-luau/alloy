--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

describe("printing", function()
	it("quotes strings inside containers and leaves the rest to tostring", function()
		expect(alloy.show("a\"b")).toBe('"a\\"b"')
		expect(alloy.show(1.5)).toBe("1.5")
		expect(alloy.show(nil)).toBe("nil")
	end)

	it("prints a struct and a variant the way the compiler asks", function()
		expect(alloy.show_struct("Vec2", { x = 1, y = 2 }, { "x", "y" })).toBe("Vec2 { x = 1, y = 2 }")
		expect(alloy.show_struct("Unit", {}, {})).toBe("Unit {}")
		expect(alloy.show_variant("Msg", { tag = "Move", _1 = 1, _2 = "up" })).toBe('Msg.Move(1, "up")')
	end)

	it("prints Result, Future, and the collections", function()
		expect(tostring(alloy.Ok(1))).toBe("Ok(1)")
		expect(tostring(alloy.Err("no"))).toBe('Err("no")')
		expect(tostring(alloy.Future.resolve(7))).toBe("Future { resolved = 7 }")
		expect(tostring(alloy.Array.from({ 1, "a" }))).toBe('[ 1, "a" ]')
		expect(tostring(alloy.Array.new())).toBe("[]")
		expect(tostring(alloy.HashMap.from({ b = 2, a = 1 }))).toBe("HashMap { a = 1, b = 2 }")
		expect(tostring(alloy.HashMap.from({ [3] = "x" }))).toBe('HashMap { [3] = "x" }')
		expect(tostring(alloy.Set.from({ 3, 1, 2 }))).toBe("Set { 1, 2, 3 }")
		expect(tostring(alloy.Queue.from({ 1, 2 }))).toBe("Queue [ 1, 2 ]")
		expect(tostring(alloy.Heap.from({ 3, 1, 2 }))).toBe("Heap [ 1, 2, 3 ]")
		local scope = alloy.Scope.new()
		scope:add(function() end)
		expect(tostring(scope)).toBe("Scope { 1 item }")
		expect(tostring(alloy.Iter.range(1, 3))).toBe("Iter { lazy }")
	end)

	it("nests: a struct inside an array inside a Result", function()
		local Vec2 = {}
		Vec2.__index = Vec2
		Vec2.__tostring = function(s)
			return alloy.show_struct("Vec2", s, { "x" })
		end
		local xs = alloy.Array.from({ setmetatable({ x = 1 }, Vec2) })
		expect(tostring(alloy.Ok(xs))).toBe("Ok([ Vec2 { x = 1 } ])")
	end)
end)
