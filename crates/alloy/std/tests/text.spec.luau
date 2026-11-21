local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local alloy = require("../alloy")

local function call(s: any, name: string, ...: any): any
	return alloy.table_call(s, name, ...)
end

describe("string methods", function()
	it("trims whitespace at each end", function()
		expect(call("  a b \n", "trim")).toBe("a b")
		expect(call("  a b ", "trim_start")).toBe("a b ")
		expect(call("  a b ", "trim_end")).toBe("  a b")
		expect(call("   ", "trim")).toBe("")
	end)

	it("tests the start, the end, and a part", function()
		expect(call("hello", "starts_with", "he")).toBe(true)
		expect(call("hello", "starts_with", "lo")).toBe(false)
		expect(call("hello", "ends_with", "lo")).toBe(true)
		expect(call("hello", "ends_with", "")).toBe(true)
		expect(call("a.b", "contains", ".")).toBe(true)
		expect(call("ab", "contains", ".")).toBe(false)
	end)

	it("pads to a width", function()
		expect(call("7", "pad_start", 3, "0")).toBe("007")
		expect(call("7", "pad_end", 3)).toBe("7  ")
		expect(call("abc", "pad_start", 2)).toBe("abc")
		expect(call("x", "pad_start", 4, "ab")).toBe("abax")
		expect(call("x", "pad_end", 4, "")).toBe("x")
	end)

	it("keeps a method of the string library and of the value", function()
		expect(call("ab", "find", "b")).toBe(2)
		expect(call(alloy.Array.from({ 1, 2 }), "contains", 2)).toBe(true)
	end)
end)
