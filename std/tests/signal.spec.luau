--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Signal = alloy.Signal

describe("Signal", function()
	it("runs handlers in connection order with the payload", function()
		local s = Signal.new()
		local seen = {}
		s:Connect(function(a, b)
			table.insert(seen, `first {a} {b}`)
		end)
		s:Connect(function(a, b)
			table.insert(seen, `second {a} {b}`)
		end)
		s:Fire(1, "x")
		expect(seen).toEqual({ "first 1 x", "second 1 x" })
	end)

	it("lets a handler disconnect another during a fire", function()
		local t = Signal.new()
		local calls = 0
		local later
		local first = t:Connect(function()
			calls += 1
			later:Disconnect()
		end)
		later = t:Connect(function()
			calls += 1
		end)
		t:Fire()
		expect(calls).toBe(1)
		expect(later.Connected).toBe(false)
		expect(first.Connected).toBe(true)
		first:Disconnect()
		t:Fire()
		expect(calls).toBe(1)
	end)

	it("Once fires one time and disconnects before its handler runs", function()
		local o = Signal.new()
		local calls = 0
		local connected_inside
		local once
		once = o:Once(function()
			calls += 1
			connected_inside = once.Connected
		end)
		o:Fire()
		o:Fire()
		expect(calls).toBe(1)
		expect(connected_inside).toBe(false)
	end)

	it("Wait resumes with the payload", function()
		local w = Signal.new()
		local got
		local thread = coroutine.create(function()
			got = { w:Wait() }
		end)
		coroutine.resume(thread)
		w:Fire("a", 2)
		expect(got).toEqual({ "a", 2 })
	end)

	it("a handler error does not stop the others", function()
		local e = Signal.new()
		local after = false
		e:Connect(function()
			error("boom")
		end)
		e:Connect(function()
			after = true
		end)
		e:Fire()
		expect(after).toBe(true)
	end)

	it("FireDeferred fires at once without a scheduler", function()
		local d = Signal.new()
		local got
		d:Connect(function(v)
			got = v
		end)
		d:FireDeferred(3)
		expect(got).toBe(3)
	end)

	it("collect drains events in order and accepts a lowercase connect", function()
		local c = Signal.new()
		local drain, connection = Signal.collect(c)
		c:Fire(1, "one")
		c:Fire(2, "two")
		expect({ drain() }).toEqual({ 1, "one" })
		expect({ drain() }).toEqual({ 2, "two" })
		expect(drain()).toBeNil()
		expect(connection.Connected).toBe(true)
		local lower = {
			connect = function(_, handler)
				handler("lower")
				return { Connected = true, Disconnect = function() end }
			end,
		}
		expect(Signal.collect(lower)()).toBe("lower")
		expect(function()
			Signal.collect({})
		end).toThrow("no Connect or connect")
	end)

	it("collect drives the generic for loop the doc writes", function()
		local c = Signal.new()
		local drain = Signal.collect(c)
		c:Fire(1, "one")
		c:Fire(2, "two")
		local seen = {}
		for id, word in drain do
			table.insert(seen, { id, word })
		end
		expect(seen).toEqual({ { 1, "one" }, { 2, "two" } })
	end)

	it("wrap follows the source until Destroy", function()
		local source = Signal.new()
		local wrapped = Signal.wrap(source)
		local got
		wrapped:Connect(function(v)
			got = v
		end)
		source:Fire(7)
		expect(got).toBe(7)
		wrapped:Destroy()
		source:Fire(8)
		expect(got).toBe(7)
		expect(Signal.is(source)).toBe(true)
		expect(Signal.is({})).toBe(false)
	end)

	it("the snake_case twins do what the Roblox names do", function()
		local s = Signal.new()
		local seen = {}
		local c = s:connect(function(n)
			table.insert(seen, n)
		end)
		s:once(function(n)
			table.insert(seen, n * 10)
		end)
		s:fire(1)
		s:fire_deferred(2)
		expect(seen).toEqual({ 1, 10, 2 })
		c:disconnect()
		expect(c.Connected).toBe(false)
		s:fire(3)
		expect(seen).toEqual({ 1, 10, 2 })
		local other = s:connect(function(n)
			table.insert(seen, n)
		end)
		s:disconnect_all()
		expect(other.Connected).toBe(false)
		s:destroy()
	end)

	it("DisconnectAll marks every connection", function()
		local d = Signal.new()
		local a = d:Connect(function() end)
		local b = d:connect(function() end)
		d:DisconnectAll()
		expect(a.Connected).toBe(false)
		expect(b.Connected).toBe(false)
	end)
end)

return nil
