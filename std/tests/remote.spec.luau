--!nonstrict
-- A remote outside Roblox. `game` is nil on this VM, so the module has
-- to load and the calls have to land somewhere.
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

local function ping()
	return alloy.remote({
		name = "Ping",
		kind = "event",
		from_client = true,
		from_server = false,
		params = { "n" },
		defaults = {},
		attrs = {},
	})
end

describe("remote", function()
	it("builds without a service", function()
		expect(ping().spec.name).toBe("Ping")
	end)

	it("records a call instead of sending it", function()
		local remote = ping()
		remote.fire(1)
		remote.fire(2)
		expect(#remote.calls).toBe(2)
		expect(remote.calls[1].name).toBe("fire")
		expect(remote.calls[2].args[1]).toBe(2)
	end)
end)
