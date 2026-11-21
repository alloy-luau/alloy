--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

local function ids(n)
	return alloy.generator(function(yield)
		for i = 1, n do
			yield(i)
		end
	end)
end

describe("generator", function()
	it("gives the yielded values to a for loop and to the Iter methods", function()
		local seen = {}
		for id in ids(3) do
			table.insert(seen, id)
		end
		expect(seen).toEqual({ 1, 2, 3 })
		expect(ids(10)
			:filter(function(x)
				return x % 2 == 0
			end)
			:to_table()).toEqual({ 2, 4, 6, 8, 10 })
	end)

	it("runs the body only as far as the reader asks", function()
		local steps = 0
		local it = alloy.generator(function(yield)
			while true do
				steps += 1
				yield(steps)
			end
		end)
		expect(steps).toBe(0)
		expect(it:take(3):to_table()).toEqual({ 1, 2, 3 })
		expect(steps).toBe(3)
	end)

	it("ends at a return, and stays at the end", function()
		local it = alloy.generator(function(yield)
			yield("a")
			do
				return
			end
		end)
		expect(it:next()).toBe("a")
		expect(it:next()).toBe(nil)
		expect(it:next()).toBe(nil)
	end)

	it("starts the body with the arguments after it", function()
		local it = alloy.generator(function(yield, ...)
			for _, v in { ... } do
				yield(v)
			end
		end, "x", "y")
		expect(it:to_table()).toEqual({ "x", "y" })
	end)

	it("hands an error of the body to the reader, then ends", function()
		local it = alloy.generator(function(yield)
			yield(1)
			error("broke", 0)
		end)
		expect(it:next()).toBe(1)
		local ok, message = pcall(it.next, it)
		expect(ok).toBe(false)
		expect(tostring(message):find("broke") ~= nil).toBe(true)
		expect(it:next()).toBe(nil)
	end)

	it("reads one generator inside another", function()
		local function twice(n)
			return alloy.generator(function(yield)
				for id in ids(n) do
					yield(id)
					yield(id)
				end
			end)
		end
		expect(twice(2):to_table()).toEqual({ 1, 1, 2, 2 })
	end)
end)
