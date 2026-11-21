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

	it("and_then chains an Ok and or_else recovers an Err", function()
		local function half(n: number)
			return if n % 2 == 0 then Ok(n // 2) else Err("odd")
		end
		expect(Ok(8):and_then(half):and_then(half)._1).toBe(2)
		expect(Ok(6):and_then(half):and_then(half)._1).toBe("odd")
		expect(Err("e"):and_then(half)._1).toBe("e")
		expect(Err("e"):or_else(function(e)
			return Ok(#e)
		end)._1).toBe(1)
		expect(Ok(3):or_else(function()
			return Ok(0)
		end)._1).toBe(3)
	end)

	it("unwrap_or_else calls its fallback on an Err only", function()
		local calls = 0
		local function fallback(e: string)
			calls += 1
			return #e
		end
		expect(Ok(2):unwrap_or_else(fallback)).toBe(2)
		expect(calls).toBe(0)
		expect(Err("abc"):unwrap_or_else(fallback)).toBe(3)
		expect(calls).toBe(1)
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

	it("maps twice down a chain", function()
		expect(Ok(2):map(function(n)
			return tostring(n * 2)
		end):map(function(s)
			return #s
		end):unwrap()).toBe(1)
		expect(Err("e"):map(function(n)
			return n
		end):map(function(n)
			return n
		end):unwrap_or(7)).toBe(7)
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

	it("pcall takes a function that returns nothing", function()
		local ran = false
		local function mark(): ()
			ran = true
		end
		local r: alloy.Result<nil, string> = Result.pcall(mark)
		expect(r:is_ok()).toBe(true)
		expect(r:ok()).toBeNil()
		expect(ran).toBe(true)
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

	it("try_block turns a throw into an Err with a traceback", function()
		local caught = alloy.try_block(function(): number
			error("kaboom")
		end)
		expect(caught:is_err()).toBe(true)
		expect(tostring(caught._1)).toContain("kaboom")
		expect(type(caught.trace)).toBe("string")
	end)

	it("try_block answers with the Err its fail raises", function()
		local raised = alloy.try_block(function(fail)
			fail("left early", "a trace")

			return 1
		end)
		expect(raised:is_err()).toBe(true)
		expect(raised._1).toBe("left early")
		expect(raised.trace).toBe("a trace")
	end)

	it("a fail leaves its own block, not the block around it", function()
		local outer = alloy.try_block(function(_fail)
			local inner = alloy.try_block(function(fail2)
				fail2("inner", nil)

				return 1
			end)

			return inner:is_err()
		end)
		expect(outer:is_ok()).toBe(true)
		expect(outer._1).toBe(true)
	end)

	it("a throw a fail did not raise still reads as a throw", function()
		local caught = alloy.try_block(function(_fail)
			error({ err = "a plain table", trace = "not the escape" })
		end)
		expect(caught:is_err()).toBe(true)
		expect(type(caught._1)).toBe("table")
		expect(caught._1.err).toBe("a plain table")
		expect(tostring(caught.trace)).toContain("table")
	end)

	it("map_or maps an Ok and gives the default for an Err", function()
		local function double(n)
			return n * 2
		end
		expect(Ok(2):map_or(0, double)).toBe(4)
		expect(Err("no"):map_or(0, double)).toBe(0)
	end)

	it("flatten removes one level", function()
		expect(Ok(Ok(1)):flatten():unwrap()).toBe(1)
		expect(Ok(Err("inner")):flatten()._1).toBe("inner")
		expect(Err("outer"):flatten()._1).toBe("outer")
		local twice = Ok(Ok(Ok(1))):flatten()
		expect(twice:unwrap():unwrap()).toBe(1)
		-- An Ok that holds no Result stays as it is.
		expect(Ok(1):flatten():unwrap()).toBe(1)
	end)

	it("all collects every Ok, or gives the first Err", function()
		local every = Result.all({ Ok(1), Ok(2), Ok(3) })
		expect(every:is_ok()).toBe(true)
		expect(every:unwrap():len()).toBe(3)
		expect(every:unwrap()[2]).toBe(2)
		local first = Err("first")
		expect(Result.all({ Ok(1), first, Err("second") })).toBe(first)
		expect(Result.all({}):unwrap():len()).toBe(0)
	end)
end)

return nil
