--!nonstrict
-- The small helpers the emit calls: `in`, spread, bind, the intrinsics,
-- the object initializer, and the test registry.
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

describe("contains", function()
	it("dispatches on the container", function()
		expect(alloy.contains("hello", "ell")).toBe(true)
		expect(alloy.contains("hello", "z")).toBe(false)
		expect(alloy.contains(alloy.Array.from({ 1, 2 }), 2)).toBe(true)
		expect(alloy.contains(alloy.Set.from({ "a" }), "a")).toBe(true)
		expect(alloy.contains(alloy.HashMap.from({ k = 1 }), "k")).toBe(true)
		expect(alloy.contains({ k = 1 }, "k")).toBe(true)
		expect(alloy.contains({ k = 1 }, "x")).toBe(false)
	end)
end)

describe("spread and bind", function()
	it("spread copies the first table and layers the rest", function()
		local base = { a = 1, b = 2 }
		local out = alloy.spread(base, { b = 3 }, nil, { c = 4 })
		expect(out).toEqual({ a = 1, b = 3, c = 4 })
		expect(base.b).toBe(2)
		expect(alloy.spread(nil, { z = 1 })).toEqual({ z = 1 })
	end)

	it("bind keeps the receiver", function()
		local obj = { n = 5 }
		local get = alloy.bind(obj, function(self, add)
			return self.n + add
		end)
		expect(get(1)).toBe(6)
	end)
end)

describe("intrinsics", function()
	it("assert_eq passes on equal and names both sides", function()
		alloy.assert_eq("f.aly:1", "a", 1, "b", 1)
		expect(function()
			alloy.assert_eq("f.aly:2", "a", 1, "b", 2)
		end).toThrow("f.aly:2: assertion failed: a == b")
	end)

	it("dbg returns the value", function()
		expect(alloy.dbg("f.aly:3", "x", 42)).toBe(42)
	end)

	it("init assigns every field and returns the object", function()
		local o = { keep = true }
		expect(alloy.init(o, { a = 1, b = "x" })).toBe(o)
		expect(o).toEqual({ keep = true, a = 1, b = "x" })
	end)
end)

describe("test registry", function()
	it("records tests in order and returns the function", function()
		local before = #alloy.tests()
		local f = function() end
		expect(alloy.test("adds", f)).toBe(f)
		local list = alloy.tests()
		expect(#list).toBe(before + 1)
		expect(list[#list].name).toBe("adds")
		expect(list[#list].run).toBe(f)
	end)
end)

return nil
