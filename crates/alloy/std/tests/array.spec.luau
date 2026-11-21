--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Array = alloy.Array

local function plain(xs)
	local out = {}
	for i, v in ipairs(xs) do
		out[i] = v
	end
	return out
end

describe("Array", function()
	it("constructs and measures", function()
		local xs = Array.from({ 1, 2, 3 })
		expect(xs:len()).toBe(3)
		expect(xs:is_empty()).toBe(false)
		expect(Array.new():is_empty()).toBe(true)
		expect(Array.with_capacity(4):len()).toBe(0)
		expect(getmetatable(xs)).toBe(getmetatable(Array.new()))
	end)

	it("pushes and pops at the end", function()
		local xs = Array.new()
		xs:push(1, 2)
		xs:push(3)
		expect(plain(xs)).toEqual({ 1, 2, 3 })
		expect(xs:pop()).toBe(3)
		expect(xs:first()).toBe(1)
		expect(xs:last()).toBe(2)
		expect(Array.new():first()).toBeNil()
	end)

	it("gives the array back, so pushes chain", function()
		local xs = Array.new()
		expect(xs:push(1):push(2, 3)).toBe(xs)
		expect(plain(xs)).toEqual({ 1, 2, 3 })
	end)

	it("pops no value from an empty array, as table.remove does", function()
		expect(select("#", Array.new():pop())).toBe(0)
	end)

	it("remove shifts the rest down; swap_remove moves the last item in", function()
		local xs = Array.from({ 1, 2, 3, 4, 5 })
		expect(xs:remove(2)).toBe(2)
		expect(plain(xs)).toEqual({ 1, 3, 4, 5 })
		expect(xs:swap_remove(1)).toBe(1)
		expect(plain(xs)).toEqual({ 5, 3, 4 })
		expect(xs:swap_remove(3)).toBe(4)
		expect(plain(xs)).toEqual({ 5, 3 })
	end)

	it("swap_remove outside the array removes nothing", function()
		local xs = Array.from({ 1, 2 })
		expect(xs:swap_remove(0)).toBeNil()
		expect(xs:swap_remove(3)).toBeNil()
		expect(xs:swap_remove(1.5)).toBeNil()
		expect(plain(xs)).toEqual({ 1, 2 })
		expect(Array.new():swap_remove(1)).toBeNil()
	end)

	it("maps, filters, and finds with the index", function()
		local xs = Array.from({ 10, 20, 30 })
		expect(plain(xs:map(function(v, i)
			return v + i
		end))).toEqual({ 11, 22, 33 })
		expect(plain(xs:filter(function(v)
			return v > 10
		end))).toEqual({ 20, 30 })
		expect(xs:find(function(v)
			return v > 15
		end)).toBe(20)
		expect(xs:find_index(function(v)
			return v > 15
		end)).toBe(2)
		expect(xs:find(function()
			return false
		end)).toBeNil()
	end)

	it("looks values up", function()
		local xs = Array.from({ "a", "b" })
		expect(xs:contains("b")).toBe(true)
		expect(xs:contains("z")).toBe(false)
		expect(xs:index_of("b")).toBe(2)
		expect(xs:index_of("z")).toBeNil()
	end)

	it("iterates and reduces in order", function()
		local seen = {}
		Array.from({ "x", "y" }):for_each(function(v, i)
			table.insert(seen, `{i}{v}`)
		end)
		expect(seen).toEqual({ "1x", "2y" })
		expect(Array.from({ 1, 2, 3 }):reduce(function(acc, v)
			return acc + v
		end, 10)).toBe(16)
	end)

	it("slices, concatenates, and reverses into new arrays", function()
		local xs = Array.from({ 1, 2, 3, 4 })
		expect(plain(xs:slice(2, 3))).toEqual({ 2, 3 })
		expect(plain(xs:slice(3))).toEqual({ 3, 4 })
		expect(plain(xs:concat(Array.from({ 5 })))).toEqual({ 1, 2, 3, 4, 5 })
		expect(plain(xs:reverse())).toEqual({ 4, 3, 2, 1 })
		expect(plain(xs)).toEqual({ 1, 2, 3, 4 })
	end)

	it("sorts in place and joins", function()
		local xs = Array.from({ 3, 1, 2 })
		expect(xs:sort_by(function(a, b)
			return a < b
		end)).toBe(xs)
		expect(plain(xs)).toEqual({ 1, 2, 3 })
		expect(xs:join(", ")).toBe("1, 2, 3")
		expect(xs:join()).toBe("123")
	end)

	it("to_table gives the array itself, so a write reaches it", function()
		local xs = Array.from({ 1, 2 })
		local t = xs:to_table()
		expect(t).toBe(xs)
		t[3] = 3
		expect(xs:len()).toBe(3)
		expect(xs:last()).toBe(3)
	end)

	it("any and all test every item", function()
		local xs = Array.from({ 1, 2, 3 })
		local function big(x)
			return x > 2
		end
		expect(xs:any(big)).toBe(true)
		expect(xs:all(big)).toBe(false)
		expect(Array.new():any(big)).toBe(false)
		expect(Array.new():all(big)).toBe(true)
	end)

	it("sums, and picks the least and the greatest by a key", function()
		local xs = Array.from({ { n = 2 }, { n = 1 }, { n = 3 }, { n = 1 } })
		local function key(x)
			return x.n
		end
		expect(Array.from({ 1, 2, 3 }):sum()).toBe(6)
		expect(Array.new():sum()).toBe(0)
		expect(xs:min_by(key)).toBe(xs[2])
		expect(xs:max_by(key)).toBe(xs[3])
		expect(Array.new():min_by(key)).toBeNil()
	end)

	it("flat_map and flatten join the parts, one level deep", function()
		local xs = Array.from({ 1, 2 })
		local out = xs:flat_map(function(x, i)
			return { x, i * 10 }
		end)
		expect(plain(out)).toEqual({ 1, 10, 2, 20 })
		expect(getmetatable(out)).toBe(Array)
		local nested = Array.from({ { 1 }, {}, { 2, { 3 } } })
		expect(plain(nested:flatten())).toEqual({ 1, 2, { 3 } })
	end)

	it("group_by keeps the order inside each group", function()
		local groups = Array.from({ 1, 2, 3, 4, 5 }):group_by(function(x)
			return if x % 2 == 0 then "even" else "odd"
		end)
		expect(plain(groups.odd)).toEqual({ 1, 3, 5 })
		expect(plain(groups.even)).toEqual({ 2, 4 })
		expect(groups.odd:len()).toBe(3)
	end)

	it("chunk cuts the array, and the last part holds the rest", function()
		local parts = Array.from({ 1, 2, 3, 4, 5 }):chunk(2)
		expect(parts:len()).toBe(3)
		expect(plain(parts[1])).toEqual({ 1, 2 })
		expect(plain(parts[3])).toEqual({ 5 })
		expect(Array.new():chunk(2):len()).toBe(0)
		expect(function()
			Array.from({ 1 }):chunk(0)
		end).toThrow("1 or more")
	end)

	it("zip stops at the shorter input", function()
		local pairs_ = Array.from({ 1, 2, 3 }):zip({ "a", "b" })
		expect(plain(pairs_)).toEqual({ { first = 1, second = "a" }, { first = 2, second = "b" } })
	end)

	it("unique keeps the first of each value", function()
		expect(plain(Array.from({ 3, 1, 3, 2, 1 }):unique())).toEqual({ 3, 1, 2 })
	end)

	it("take and skip split at a count, and a count past the end is safe", function()
		local xs = Array.from({ 1, 2, 3 })
		expect(plain(xs:take(2))).toEqual({ 1, 2 })
		expect(plain(xs:skip(2))).toEqual({ 3 })
		expect(plain(xs:take(9))).toEqual({ 1, 2, 3 })
		expect(plain(xs:skip(9))).toEqual({})
		expect(plain(xs:take(0))).toEqual({})
		expect(plain(xs:skip(0))).toEqual({ 1, 2, 3 })
		expect(plain(xs)).toEqual({ 1, 2, 3 })
	end)

	it("insert_at moves the later items up and chains", function()
		local xs = Array.from({ 1, 3 })
		expect(xs:insert_at(2, 2):insert_at(1, 0)).toBe(xs)
		expect(plain(xs)).toEqual({ 0, 1, 2, 3 })
	end)

	it("binary_search finds an item of a sorted array", function()
		local xs = Array.from({ 1, 3, 5, 7 })
		for i, v in ipairs(xs) do
			expect(xs:binary_search(v)).toBe(i)
		end
		expect(xs:binary_search(4)).toBeNil()
		expect(xs:binary_search(0)).toBeNil()
		expect(xs:binary_search(9)).toBeNil()
		expect(Array.new():binary_search(1)).toBeNil()
		local down = Array.from({ 7, 5, 3, 1 })
		expect(down:binary_search(3, function(a, b)
			return a > b
		end)).toBe(3)
	end)

	it("range counts from one end to the other, both included", function()
		expect(plain(Array.range(1, 4))).toEqual({ 1, 2, 3, 4 })
		expect(plain(Array.range(1, 10, 4))).toEqual({ 1, 5, 9 })
		expect(plain(Array.range(3, 1, -1))).toEqual({ 3, 2, 1 })
		expect(plain(Array.range(2, 1))).toEqual({})
		expect(Array.range(1, 3):len()).toBe(3)
	end)

	it("the free form takes a plain table", function()
		local raw = { 1, 2, 2, 3 }
		expect(Array.sum(raw)).toBe(8)
		expect(plain(Array.unique(raw))).toEqual({ 1, 2, 3 })
		expect(plain(Array.take(raw, 1))).toEqual({ 1 })
		expect(Array.any(raw, function(x)
			return x == 3
		end)).toBe(true)
		expect(getmetatable(raw)).toBeNil()
	end)
end)

return nil
