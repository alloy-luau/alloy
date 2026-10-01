--!nonstrict
local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local alloy = require("../alloy")

describe("message with a reply", function()
	it("sends the event after the arguments, nil before any replied", function()
		local sent
		local actor = {
			SendMessage = function(_, ...)
				sent = table.pack(...)
			end,
		}
		local Light = alloy.message("Light", 2)
		Light.fire(actor, 7)
		expect(sent.n).toBe(4)
		expect(sent[1]).toBe("Light")
		expect(sent[2]).toBe(7)
		expect(sent[3]).toBe(nil)
		expect(sent[4]).toBe(nil)
	end)

	it("gives the handler respond in place of the event", function()
		local fired
		local event = {
			Fire = function(_, ...)
				fired = table.pack(...)
			end,
		}
		local got
		local handler = alloy.message_respond(function(job, input, respond)
			got = { job, input }
			respond(job, "levels")
		end, 2, false)
		handler(7, "input", event)
		expect(got[1]).toBe(7)
		expect(got[2]).toBe("input")
		expect(fired.n).toBe(2)
		expect(fired[1]).toBe(7)
		expect(fired[2]).toBe("levels")
	end)

	it("makes respond do nothing when no script listens", function()
		local ran = false
		local handler = alloy.message_respond(function(_, respond)
			respond(1)
			ran = true
		end, 1, false)
		handler(1, nil)
		expect(ran).toBe(true)
	end)
end)
