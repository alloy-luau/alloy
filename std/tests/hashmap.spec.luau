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
end)

return nil
