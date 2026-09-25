--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local BitSet = alloy.BitSet

local function members(bits)
	local out = {}
	for i in bits do
		table.insert(out, i)
	end
	return out
end

describe("BitSet", function()
	it("adds, finds, removes, and counts", function()
		local bits = BitSet.new()
		expect(bits:is_empty()).toBe(true)
		expect(bits:add(3)).toBe(bits)
		bits:add(3):add(0)
		expect(bits:len()).toBe(2)
		expect(bits:has(3)).toBe(true)
		expect(bits:has(4)).toBe(false)
		expect(bits:remove(3)).toBe(true)
		expect(bits:remove(3)).toBe(false)
		expect(bits:len()).toBe(1)
	end)

	it("grows past its size and walks the members in rising order", function()
		local bits = BitSet.new(8)
		for _, i in { 1000, 31, 0, 32, 5, 70000 } do
			bits:add(i)
		end
		expect(members(bits)).toEqual({ 0, 5, 31, 32, 1000, 70000 })
		expect(bits:to_array()).toEqual({ 0, 5, 31, 32, 1000, 70000 })
		expect(bits:len()).toBe(6)
	end)

	it("finds nothing below 0, past the end, or with a fraction", function()
		local bits = BitSet.from({ 1, 2 })
		expect(bits:has(-1)).toBe(false)
		expect(bits:has(1.5)).toBe(false)
		expect(bits:has(1e9)).toBe(false)
		expect(bits:remove(-1)).toBe(false)
		expect(bits:remove(1e9)).toBe(false)
	end)

	it("refuses to add a number that is not whole or is below 0", function()
		local bits = BitSet.new()
		expect(function()
			bits:add(-1)
		end).toThrow()
		expect(function()
			bits:add(2.5)
		end).toThrow()
		expect(bits:len()).toBe(0)
	end)

	it("combines sets a word at a time into new ones", function()
		local a = BitSet.from({ 1, 2, 3, 40 })
		local b = BitSet.from({ 3, 4, 5000 })
		expect(members(a:union(b))).toEqual({ 1, 2, 3, 4, 40, 5000 })
		expect(a:union(b):len()).toBe(6)
		expect(members(a:intersection(b))).toEqual({ 3 })
		expect(b:intersection(a):len()).toBe(1)
		expect(members(a:difference(b))).toEqual({ 1, 2, 40 })
		expect(members(b:difference(a))).toEqual({ 4, 5000 })
		expect(a:len()).toBe(4)
		expect(b:len()).toBe(3)
	end)

	it("clear empties it and prints its members", function()
		local bits = BitSet.from({ 9, 2 })
		expect(tostring(bits)).toBe("BitSet { 2, 9 }")
		bits:clear()
		expect(bits:is_empty()).toBe(true)
		expect(bits:has(9)).toBe(false)
		expect(tostring(bits)).toBe("BitSet {}")
	end)

	it("agrees with a Set over many random members", function()
		local bits = BitSet.new(1)
		local set = alloy.Set.new()
		local seed = 7
		for _ = 1, 2000 do
			seed = (seed * 1103515245 + 12345) % 2147483648
			local i = seed % 5000
			if seed % 3 == 0 then
				expect(bits:remove(i)).toBe(set:remove(i))
			else
				bits:add(i)
				set:add(i)
			end
		end
		expect(bits:len()).toBe(set:len())
		local sorted = set:to_array()
		table.sort(sorted)
		expect(members(bits)).toEqual(sorted)
		expect(bits:union(BitSet.new()):len()).toBe(set:len())
	end)
end)

return nil
