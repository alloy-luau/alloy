--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Heap = alloy.Heap

describe("Heap", function()
	it("pops the least value first", function()
		local h = Heap.from({ 5, 1, 4, 2, 3 })
		expect(h:len()).toBe(5)
		expect(h:peek()).toBe(1)
		local out = {}
		while not h:is_empty() do
			table.insert(out, h:pop())
		end
		expect(out).toEqual({ 1, 2, 3, 4, 5 })
		expect(h:pop()).toBeNil()
	end)

	it("orders tables by the comparator", function()
		local h = Heap.new(function(a, b)
			return a.cost < b.cost
		end)
		h:push({ name = "far", cost = 9 })
		h:push({ name = "near", cost = 1 })
		h:push({ name = "mid", cost = 4 })
		expect(h:pop().name).toBe("near")
		expect(h:pop().name).toBe("mid")
		expect(h:to_array():map(function(v)
			return v.name
		end)).toEqual({ "far" })
	end)

	it("iterates in order without popping", function()
		local h = Heap.from({ 3, 1, 2 })
		local seen = {}
		for v in h do
			table.insert(seen, v)
		end
		expect(seen).toEqual({ 1, 2, 3 })
		expect(h:len()).toBe(3)
		h:clear()
		expect(h:is_empty()).toBe(true)
	end)

	it("holds the heap shape under many pushes", function()
		local h = Heap.new()
		local values = {}
		for i = 1, 200 do
			local v = (i * 7919) % 101
			table.insert(values, v)
			h:push(v)
		end
		table.sort(values)
		expect(h:to_array()).toEqual(values)
		local last = -1
		for _ = 1, 200 do
			local v = h:pop()
			expect(v >= last).toBe(true)
			last = v
		end
	end)

	it("pops in order from from() and from pushes, with or without a comparator", function()
		local values = {}
		for i = 1, 300 do
			values[i] = (i * 7919) % 211
		end
		local sorted = table.clone(values)
		table.sort(sorted)
		local desc = table.clone(values)
		table.sort(desc, function(a, b)
			return a > b
		end)
		local greater = function(a, b)
			return a > b
		end
		local built = { Heap.from(values), Heap.from(values, greater), Heap.new(), Heap.new(greater) }
		for _, v in values do
			built[3]:push(v)
			built[4]:push(v)
		end
		for k, h in built do
			local want = if k % 2 == 1 then sorted else desc
			local out = {}
			while not h:is_empty() do
				table.insert(out, h:pop())
			end
			expect(out).toEqual(want)
		end
	end)

	it("to_table gives the live table, in heap order", function()
		local h = Heap.from({ 3, 1, 2 })
		local t = h:to_table()
		expect(t[1]).toBe(1)
		expect(#t).toBe(3)
		t[#t + 1] = 0
		-- The write reaches the heap and skips the sift, so the order breaks.
		expect(h:len()).toBe(4)
		expect(h:peek()).toBe(1)
	end)
end)
