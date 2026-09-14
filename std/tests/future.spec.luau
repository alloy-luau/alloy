--!nonstrict
-- The test VM has no `task`, so a Future settles at once: the runtime's
-- coroutine fallback runs its body before `future` returns.
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Future = alloy.Future

describe("Future", function()
	it("runs its body and awaits the value", function()
		local f = alloy.future(function(a, b)
			return a + b
		end, 2, 3)
		expect(f:is_settled()).toBe(true)
		expect(alloy.await(f)).toBe(5)
	end)

	it("resolve and reject settle without a body", function()
		expect(alloy.await(Future.resolve("r"))).toBe("r")
		expect(function()
			alloy.await(Future.reject("no"))
		end).toThrow("no")
	end)

	it("try_await turns a failure into an Err", function()
		expect(alloy.try_await(Future.resolve(1)):unwrap()).toBe(1)
		local failed = alloy.try_await(Future.reject("bad"))
		expect(failed:is_err()).toBe(true)
		expect(tostring(failed._1)).toContain("bad")
	end)

	it("try_await_result unwraps the Result a Future settles with", function()
		local ok = alloy.try_await_result(Future.resolve(alloy.Ok(1)))
		expect(ok:is_ok()).toBe(true)
		expect(ok:unwrap()).toBe(1)
		local err = alloy.try_await_result(Future.resolve(alloy.Err("no")))
		expect(err:is_err()).toBe(true)
		expect(err._1).toBe("no")
	end)

	it("an error inside the body rejects with its message", function()
		local f = alloy.future(function()
			error("inside")
		end)
		expect(function()
			alloy.await(f)
		end).toThrow("inside")
	end)

	-- The stored trace opens with the message, so a rethrow of both
	-- printed the failure twice and the report read nothing like the
	-- one a plain call gives.
	it("rethrows the value alone, with the message once", function()
		local f = alloy.future(function()
			error("boom")
		end)
		local ok, thrown = pcall(alloy.await, f)
		expect(ok).toBe(false)
		expect(thrown).toBe(f.value)
		expect(type(f.trace)).toBe("string")
		local _, count = string.gsub(tostring(thrown), "boom", "")
		expect(count).toBe(1)
	end)

	it("all collects in order, race and any pick the first", function()
		local all = Future.all({ Future.resolve(1), Future.resolve(2) })
		expect(alloy.await(all)).toEqual({ 1, 2 })
		expect(alloy.await(Future.race({ Future.resolve("a"), Future.resolve("b") }))).toBe("a")
		expect(alloy.await(Future.any({ Future.reject("x"), Future.resolve("ok") }))).toBe("ok")
	end)

	it("races a list whose futures carry different values", function()
		local ready = Future.resolve(42)
		local timer = Future.delay(0)
		expect(alloy.await(Future.race({ ready, timer }))).toBe(42)
	end)

	it("any rejects when every future fails", function()
		local f = Future.any({ Future.reject("one"), Future.reject("two") })
		expect(function()
			alloy.await(f)
		end).toThrow("two")
	end)

	it("all and all_settled hand back an Array", function()
		local values = alloy.await(Future.all({ Future.resolve(1), Future.resolve(2) }))
		expect(getmetatable(values)).toBe(alloy.Array)
		expect(values:len()).toBe(2)
		expect(values:map(function(n)
			return n * 2
		end):join(",")).toBe("2,4")
		local settled = alloy.await(Future.all_settled({ Future.resolve(1) }))
		expect(getmetatable(settled)).toBe(alloy.Array)
		expect(settled:len()).toBe(1)
	end)

	it("all_settled keeps both sides as Results", function()
		local settled = alloy.await(Future.all_settled({ Future.resolve(1), Future.reject("e") }))
		expect(settled[1]:unwrap()).toBe(1)
		expect(settled[2]:is_err()).toBe(true)
	end)

	it("delay settles after the wait with no value", function()
		local f = Future.delay(0.5)
		expect(f:is_settled()).toBe(true)
		expect(alloy.await(f)).toBeNil()
	end)

	it("cancel settles a pending future", function()
		local f = Future.resolve(1)
		f:cancel()
		expect(f:is_settled()).toBe(true)
	end)

	it("andThen gets the value or the error", function()
		local got
		Future.resolve(7):andThen(function(v)
			got = v
		end)
		expect(got).toBe(7)
		local err
		Future.reject("why"):andThen(nil, function(e)
			err = e
		end)
		expect(err).toBe("why")
	end)

	it("and_then runs what andThen runs", function()
		local seen
		Future.resolve(3):and_then(function(v)
			seen = v
		end)
		expect(seen).toBe(3)
	end)
end)

return nil
