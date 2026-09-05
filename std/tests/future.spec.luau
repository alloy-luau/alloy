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

	it("an error inside the body rejects with its message", function()
		local f = alloy.future(function()
			error("inside")
		end)
		expect(function()
			alloy.await(f)
		end).toThrow("inside")
	end)

	it("all collects in order, race and any pick the first", function()
		local all = Future.all({ Future.resolve(1), Future.resolve(2) })
		expect(alloy.await(all)).toEqual({ 1, 2 })
		expect(alloy.await(Future.race({ Future.resolve("a"), Future.resolve("b") }))).toBe("a")
		expect(alloy.await(Future.any({ Future.reject("x"), Future.resolve("ok") }))).toBe("ok")
	end)

	it("any rejects when every future fails", function()
		local f = Future.any({ Future.reject("one"), Future.reject("two") })
		expect(function()
			alloy.await(f)
		end).toThrow("two")
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
end)

return nil
