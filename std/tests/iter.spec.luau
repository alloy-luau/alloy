--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Iter, Queue = alloy.Iter, alloy.Queue

describe("Iter", function()
	it("chains lazily and collects", function()
		local pulled = 0
		local it = Iter.from({ 1, 2, 3, 4, 5, 6 })
			:map(function(v)
				pulled += 1
				return v * 10
			end)
			:filter(function(v)
				return v % 20 == 0
			end)
			:take(2)
		expect(pulled).toBe(0)
		expect(it:collect()).toEqual({ 20, 40 })
		expect(pulled).toBe(4)
	end)

	it("counts a range, skips, and takes while", function()
		expect(Iter.range(1, 5):collect()).toEqual({ 1, 2, 3, 4, 5 })
		expect(Iter.range(10, 1, -3):collect()).toEqual({ 10, 7, 4, 1 })
		expect(Iter.range(1, 10):skip(7):collect()).toEqual({ 8, 9, 10 })
		expect(Iter.range(1, 10):take_while(function(v)
			return v < 4
		end):collect()).toEqual({ 1, 2, 3 })
		expect(Iter.range(1, 100):count()).toBe(100)
	end)

	it("folds, searches, and answers any and all", function()
		local it = Iter.from({ 2, 4, 6 })
		expect(it:all(function(v)
			return v % 2 == 0
		end)).toBe(true)
		expect(Iter.from({ 2, 3 }):any(function(v)
			return v == 3
		end)).toBe(true)
		expect(Iter.from({ 2, 3 }):find(function(v)
			return v > 2
		end)).toBe(3)
		expect(Iter.from({ 1, 2, 3 }):reduce(function(acc, v)
			return acc + v
		end, 0)).toBe(6)
		expect(Iter.from({ 7, 8 }):first()).toBe(7)
		expect(Iter.from({ 7, 8 }):last()).toBe(8)
		expect(Iter.from({}):first()).toBeNil()
	end)

	it("reads a function, a queue, and a for loop", function()
		local n = 0
		local from_fn = Iter.from(function()
			n += 1
			if n > 3 then
				return nil
			end
			return n
		end)
		expect(from_fn:collect()).toEqual({ 1, 2, 3 })
		local q = Queue.from({ "a", "b" })
		expect(Iter.from(q):chain(Iter.from({ "c" })):collect()).toEqual({ "a", "b", "c" })
		local seen = {}
		for v in Iter.range(1, 3) do
			table.insert(seen, v)
		end
		expect(seen).toEqual({ 1, 2, 3 })
	end)
end)
