--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Ok, Err, Result = alloy.Ok, alloy.Err, alloy.Result

describe("Result", function()
	it("unwraps an Ok and throws on an Err", function()
		expect(Ok(1):unwrap()).toBe(1)
		expect(function()
			Err("boom"):unwrap()
		end).toThrow("boom")
	end)

	it("expect names the failure", function()
		expect(Ok("v"):expect("no")).toBe("v")
		expect(function()
			Err("bad"):expect("needed a value")
		end).toThrow("needed a value")
	end)

	it("unwrap_or falls back on an Err only", function()
		expect(Ok(2):unwrap_or(9)).toBe(2)
		expect(Err("e"):unwrap_or(9)).toBe(9)
	end)

	it("map touches the Ok side and map_err the Err side", function()
		expect(Ok(2):map(function(v)
			return v * 3
		end):unwrap()).toBe(6)
		expect(Err("e"):map(function(v)
			return v * 3
		end):is_err()).toBe(true)
		expect(Err("e"):map_err(string.upper):unwrap_or("x")).toBe("x")
		expect(Err("e"):map_err(string.upper)._1).toBe("E")
		expect(Ok(1):map_err(string.upper):unwrap()).toBe(1)
	end)

	it("reports its side", function()
		expect(Ok(1):is_ok()).toBe(true)
		expect(Ok(1):is_err()).toBe(false)
		expect(Err(1):is_ok()).toBe(false)
		expect(Err(1):is_err()).toBe(true)
		expect(Ok(5):ok()).toBe(5)
		expect(Err(5):ok()).toBeNil()
	end)

	it("pcall wraps a throwing call with its trace", function()
		local good = Result.pcall(function(a, b)
			return a + b
		end, 1, 2)
		expect(good:unwrap()).toBe(3)
		local bad = Result.pcall(function()
			error("split")
		end)
		expect(bad:is_err()).toBe(true)
		expect(tostring(bad._1)).toContain("split")
	end)

	it("try_block wraps a plain value and passes a Result through", function()
		expect(alloy.try_block(function()
			return 4
		end):unwrap()).toBe(4)
		local passed = alloy.try_block(function()
			return Err("kept")
		end)
		expect(passed:is_err()).toBe(true)
		expect(passed._1).toBe("kept")
	end)
end)

return nil
