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

	it("counts the sets it builds and leaves the inputs alone", function()
		local small = Set.from({ 1, 2 })
		local big = Set.from({ 2, 3, 4, 5 })
		local u = small:union(big)
		expect(u:len()).toBe(5)
		expect(big:union(small):len()).toBe(5)
		u:add(9)
		expect(big:has(9) or small:has(9)).toBe(false)
		expect(small:intersection(big):len()).toBe(1)
		expect(big:difference(small):len()).toBe(3)
		expect(Set.from({ 1, 1, 1 }):len()).toBe(1)
	end)

	it("iterates its members in a for loop, not its own fields", function()
		local seen = {}
		for v in Set.from({ "a", "b" }) do
			table.insert(seen, v)
		end
		table.sort(seen)
		expect(seen).toEqual({ "a", "b" })
	end)

	it("to_array gives an Array", function()
		local xs = Set.from({ 5 }):to_array()
		expect(xs:len()).toBe(1)
		expect(xs:contains(5)).toBe(true)
	end)

	it("to_table gives the live table of keys, so a write reaches the set", function()
		local s = Set.from({ 1 })
		local t = s:to_table()
		expect(t).toEqual({ [1] = true })
		t[2] = true
		expect(s:has(2)).toBe(true)
		-- The write skips the count, which `add` keeps.
		expect(s:len()).toBe(1)
	end)
end)

return nil
