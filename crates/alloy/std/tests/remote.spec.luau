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

	it("raises at the fire when a value does not fit its width", function()
		local heal = alloy.remote({
			name = "Heal",
			kind = "event",
			from_client = true,
			from_server = false,
			params = { "amount" },
			defaults = {},
			attrs = {},
			wire = { "u8" },
		})
		expect(function()
			heal.fire(300)
		end).toThrow("Heal: argument 1: 300 does not fit @u8")
		heal.fire(3)
		expect(#heal.calls).toBe(1)

		-- The server's fire carries the player before the arguments.
		local down = alloy.remote({
			name = "Down",
			kind = "event",
			from_client = false,
			from_server = true,
			params = { "amount" },
			defaults = {},
			attrs = {},
			wire = { "u8" },
		})
		down.fire({ Name = "p" }, 3)
		expect(function()
			down.fire({ Name = "p" }, 300)
		end).toThrow("Down: argument 1: 300 does not fit @u8")
		expect(#down.calls).toBe(1)
	end)
end)

-- A server with just enough engine for a remote: the services the
-- runtime asks for, and a RemoteEvent whose `deliver` plays one client
-- fire to every connection, as the engine does.
local function server_world()
	local function new_instance(class)
		local self = { ClassName = class, kids = {} }
		function self:FindFirstChild(name)
			return self.kids[name]
		end
		local connections = {}
		self.OnServerEvent = {
			Connect = function(_, fn)
				table.insert(connections, fn)
				return {
					Disconnect = function()
						table.remove(connections, table.find(connections, fn))
					end,
				}
			end,
		}
		self.connections = connections
		function self.deliver(player, ...)
			for _, fn in table.clone(connections) do
				fn(player, ...)
			end
		end
		return setmetatable(self, {
			__newindex = function(t, k, v)
				rawset(t, k, v)
				if k == "Parent" and v ~= nil then
					v.kids[rawget(t, "Name")] = t
				end
			end,
		})
	end
	local leaving = {}
	local services = {
		RunService = {
			IsServer = function()
				return true
			end,
		},
		ReplicatedStorage = new_instance("ReplicatedStorage"),
		Players = {
			PlayerRemoving = {
				Connect = function(_, fn)
					table.insert(leaving, fn)
				end,
			},
		},
	}
	_G.game = {
		GetService = function(_, name)
			return services[name]
		end,
	}
	_G.Instance = { new = new_instance }
	return function(player)
		for _, fn in leaving do
			fn(player)
		end
	end
end

local function leave_world()
	_G.game = nil
	_G.Instance = nil
end

local function event(name, attrs, wire)
	return alloy.remote({
		name = name,
		kind = "event",
		from_client = true,
		from_server = false,
		params = { "n" },
		defaults = {},
		attrs = attrs,
		wire = wire,
	})
end

describe("remote on a server", function()
	it("drops a packed packet longer than its layout", function()
		server_world()
		local seen = {}
		local remote = event("Packed", {}, { "u8" })
		remote.on(function(_, n)
			table.insert(seen, n)
		end)
		local good = buffer.create(1)
		buffer.writeu8(good, 0, 5)
		local long = buffer.create(2)
		buffer.writeu8(long, 0, 6)
		remote.instance.deliver({}, good)
		remote.instance.deliver({}, long)
		leave_world()
		expect(#seen).toBe(1)
		expect(seen[1]).toBe(5)
	end)

	it("charges a message once, however many handlers listen", function()
		local leave = server_world()
		local remote = event("Limited", { ratelimit = { 3, 60 } })
		local first, second, limited = 0, 0, 0
		remote.on(function()
			first += 1
		end)
		remote.on(function()
			second += 1
		end)
		remote.on_ratelimited(function()
			limited += 1
		end)
		expect(#remote.instance.connections).toBe(1)
		local a, b = { Name = "a" }, { Name = "b" }
		for _ = 1, 4 do
			remote.instance.deliver(a, 1)
		end
		-- Three pass to both handlers; the fourth is over the limit.
		expect(first).toBe(3)
		expect(second).toBe(3)
		expect(limited).toBe(1)
		-- Another sender has a bucket of its own.
		remote.instance.deliver(b, 1)
		expect(first).toBe(4)
		-- A player who leaves takes the bucket along.
		leave(a)
		remote.instance.deliver(a, 1)
		leave_world()
		expect(first).toBe(5)
		expect(second).toBe(5)
	end)

	it("keeps the other handlers when one disconnects", function()
		server_world()
		local remote = event("Shared", {})
		local first, second = 0, 0
		local connection = remote.on(function()
			first += 1
		end)
		remote.on(function()
			second += 1
		end)
		remote.instance.deliver({}, 1)
		connection:Disconnect()
		remote.instance.deliver({}, 1)
		leave_world()
		expect(first).toBe(1)
		expect(second).toBe(2)
	end)
end)
