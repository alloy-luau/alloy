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

	-- `async function pair() return 1, 2 end`: the Future kept only the
	-- first value, and `local a, b = await pair()` read `b` as nil.
	it("keeps every value the body returns", function()
		local f = alloy.future(function()
			return 1, nil, 3
		end)
		local a, b, c = alloy.await(f)
		expect(a).toBe(1)
		expect(b).toBeNil()
		expect(c).toBe(3)
		expect(select("#", alloy.await(f))).toBe(3)
		local seen
		f:andThen(function(...)
			seen = table.pack(...)
		end)
		expect(seen.n).toBe(3)
		expect(seen[3]).toBe(3)
		local x, y = alloy.await(Future.race({ alloy.future(function()
			return "a", "b"
		end) }))
		expect(x).toBe("a")
		expect(y).toBe("b")
	end)

	it("awaits every value a hand-written andThen gives", function()
		local obj = {
			andThen = function(_, on_resolve)
				on_resolve("p", "q")
			end,
		}
		-- The foreign path resumes the thread that waits, so it runs on one.
		local p, q = alloy.await(alloy.future(function()
			return alloy.await(obj)
		end))
		expect(p).toBe("p")
		expect(q).toBe("q")
	end)

	-- A body that yields and is never resumed: the Future stays pending.
	local function pending()
		return alloy.future(function()
			coroutine.yield()
		end)
	end

	it("timeout passes a settled Future through", function()
		expect(alloy.await(Future.timeout(Future.resolve(7), 1))).toBe(7)
		expect(function()
			alloy.await(Future.timeout(Future.reject("bad"), 1))
		end).toThrow("bad")
		local a, b = alloy.await(Future.timeout(
			alloy.future(function()
				return "a", "b"
			end),
			1
		))
		expect(a).toBe("a")
		expect(b).toBe("b")
	end)

	-- A delay runs at once in this VM, so a pending Future times out at once.
	it("timeout rejects when the Future is still pending", function()
		local f = Future.timeout(pending(), 1)
		expect(f:is_settled()).toBe(true)
		expect(function()
			alloy.await(f)
		end).toThrow("timeout")
	end)

	it("retry calls again after an error, up to the count", function()
		local calls = 0
		local f = Future.retry(function()
			calls += 1
			if calls < 3 then
				error("not yet")
			end
			return calls
		end, 5, 0.1)
		expect(alloy.await(f)).toBe(3)
		expect(calls).toBe(3)
	end)

	it("retry rejects with the last error", function()
		local calls = 0
		local f = Future.retry(function()
			calls += 1
			error(`try {calls}`, 0)
		end, 2)
		expect(calls).toBe(2)
		local ok, err = pcall(alloy.await, f)
		expect(ok).toBe(false)
		expect(err).toBe("try 2")
	end)

	it("finally runs on a value, on a failure, and on a cancel", function()
		local ran = 0
		local function count()
			ran += 1
		end
		local ok = Future.resolve(1)
		expect(ok:finally(count)).toBe(ok)
		Future.reject("no"):finally(count)
		expect(ran).toBe(2)
		local waits = pending()
		waits:finally(count)
		expect(ran).toBe(2)
		waits:cancel()
		expect(ran).toBe(3)
	end)

	it("from_signal settles with the next event and disconnects", function()
		local signal = alloy.Signal.new()
		local f = Future.from_signal(signal)
		expect(f:is_settled()).toBe(false)
		signal:Fire("a", 2)
		local name, n = alloy.await(f)
		expect(name).toBe("a")
		expect(n).toBe(2)
		expect(signal._head).toBeNil()
		signal:Fire("b", 3)
		expect(alloy.await(f)).toBe("a")
	end)

	it("from_signal takes any source with connect", function()
		local handler
		local disconnected = false
		local source = {
			connect = function(_, h)
				handler = h
				return {
					disconnect = function()
						disconnected = true
					end,
				}
			end,
		}
		local f = Future.from_signal(source)
		handler(5)
		expect(alloy.await(f)).toBe(5)
		expect(disconnected).toBe(true)
	end)

	-- A delay runs at once in this VM, so each call reaches `f`. The
	-- test holds the arguments and the wrapper shape.
	it("debounce passes the arguments of the call", function()
		local seen = {}
		local save = Future.debounce(function(a, b)
			table.insert(seen, a + b)
		end, 0.5)
		expect(save(1, 2)).toBeNil()
		save(3, 4)
		expect(seen).toEqual({ 3, 7 })
	end)

	it("throttle drops a call inside the window", function()
		local calls = {}
		local fire = Future.throttle(function(v)
			table.insert(calls, v)
		end, 1000)
		fire("a")
		fire("b")
		fire("c")
		expect(calls).toEqual({ "a" })
		local every = Future.throttle(function(v)
			table.insert(calls, v)
		end, 0)
		every("d")
		every("e")
		expect(calls).toEqual({ "a", "d", "e" })
	end)
end)

return nil
