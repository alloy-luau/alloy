--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local HashMap = alloy.HashMap

describe("HashMap", function()
	it("sets, gets, and counts", function()
		local m = HashMap.new()
		expect(m:set("a", 1)).toBe(m)
		m:set("b", 2)
		expect(m:get("a")).toBe(1)
		expect(m:has("b")).toBe(true)
		expect(m:has("c")).toBe(false)
		expect(m:len()).toBe(2)
	end)

	it("gives the map back, so sets chain", function()
		local m = HashMap.new()
		expect(m:set("a", 1):set("b", 2)).toBe(m)
		expect(m:len()).toBe(2)
	end)

	it("counts a replaced and a removed entry once", function()
		local m = HashMap.from({ a = 1 })
		m:set("a", 5)
		expect(m:len()).toBe(1)
		m:set("a", nil)
		expect(m:len()).toBe(0)
		m:set("k", 1)
		expect(m:remove("k")).toBe(1)
		expect(m:remove("k")).toBeNil()
		expect(m:len()).toBe(0)
	end)

	it("lists keys, values, and entries", function()
		local m = HashMap.from({ x = 10, y = 20 })
		local keys = m:keys()
		expect(keys:len()).toBe(2)
		expect(keys:contains("x") and keys:contains("y")).toBe(true)
		local values = m:values()
		expect(values:contains(10) and values:contains(20)).toBe(true)
		local seen = {}
		for k, v in m:entries() do
			seen[k] = v
		end
		expect(seen).toEqual({ x = 10, y = 20 })
	end)

	it("iterates its pairs in a for loop, not its own fields", function()
		local m = HashMap.from({ x = 10, y = 20 })
		local seen = {}
		for k, v in m do
			seen[k] = v
		end
		expect(seen).toEqual({ x = 10, y = 20 })
	end)

	it("from copies a table and leaves its metatable behind", function()
		local source = setmetatable({ a = 1, b = 2 }, { __index = function()
			return 0
		end })
		local m = HashMap.from(source)
		expect(m:len()).toBe(2)
		expect(getmetatable(m:to_table())).toBeNil()
		m:set("c", 3)
		expect(rawget(source, "c")).toBeNil()
		local plain = { k = true }
		local copy = HashMap.from(plain)
		copy:set("z", true)
		expect(plain.z).toBeNil()
	end)

	it("get_or_insert stores the default one time", function()
		local m = HashMap.new()
		expect(m:get_or_insert("n", 1)).toBe(1)
		expect(m:get_or_insert("n", 2)).toBe(1)
		expect(m:len()).toBe(1)
	end)

	it("clear empties it", function()
		local m = HashMap.from({ a = 1, b = 2 })
		m:clear()
		expect(m:len()).toBe(0)
		expect(m:get("a")).toBeNil()
	end)

	it("to_table gives the live table, so a write reaches the map", function()
		local m = HashMap.from({ a = 1 })
		local t = m:to_table()
		expect(t).toEqual({ a = 1 })
		t.b = 2
		expect(m:get("b")).toBe(2)
		-- The write skips the count, which `set` keeps.
		expect(m:len()).toBe(1)
	end)
end)

return nil
