--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Set = alloy.Set

local function sorted(xs)
	local out = {}
	for _, v in ipairs(xs) do
		table.insert(out, v)
	end
	table.sort(out)
	return out
end

describe("Set", function()
	it("adds each value one time", function()
		local s = Set.new()
		expect(s:add(1)).toBe(s)
		s:add(1)
		s:add(2)
		expect(s:len()).toBe(2)
		expect(s:has(1)).toBe(true)
		expect(s:has(3)).toBe(false)
	end)

	it("removes and reports whether it did", function()
		local s = Set.from({ "a", "b" })
		expect(s:remove("a")).toBe(true)
		expect(s:remove("a")).toBe(false)
		expect(s:len()).toBe(1)
	end)

	it("combines sets into new ones", function()
		local a = Set.from({ 1, 2, 3 })
		local b = Set.from({ 3, 4 })
		expect(sorted(a:union(b):to_array())).toEqual({ 1, 2, 3, 4 })
		expect(sorted(a:intersection(b):to_array())).toEqual({ 3 })
		expect(sorted(a:difference(b):to_array())).toEqual({ 1, 2 })
		expect(a:len()).toBe(3)
	end)

	it("to_array gives an Array", function()
		local xs = Set.from({ 5 }):to_array()
		expect(xs:len()).toBe(1)
		expect(xs:contains(5)).toBe(true)
	end)
end)

return nil
