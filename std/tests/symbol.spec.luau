--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

describe("Symbol", function()
	it("is unique, frozen, and prints its name", function()
		local a = alloy.Symbol.new("key")
		local b = alloy.Symbol.new("key")
		expect(a == b).toBe(false)
		expect(tostring(a)).toBe("Symbol(key)")
		expect(table.isfrozen(a)).toBe(true)
		local t = {}
		t[a] = 1
		expect(t[b]).toBeNil()
		expect(t[a]).toBe(1)
	end)
end)

return nil
