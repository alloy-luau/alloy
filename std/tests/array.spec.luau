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
end)

return nil
