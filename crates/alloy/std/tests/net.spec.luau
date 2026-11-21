--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Net = alloy.Net

-- A stand-in for HttpService. `answer` gets each request and returns a
-- response or raises. The JSON here is a marker and a table of values.
local function service(answer)
	local self = { calls = {}, values = {} }
	function self:RequestAsync(request)
		table.insert(self.calls, request)
		return answer(request, #self.calls)
	end
	function self:JSONEncode(value)
		if type(value) == "function" then
			error("Can't convert to JSON")
		end
		table.insert(self.values, value)
		return "json#" .. #self.values
	end
	function self:JSONDecode(text)
		local at = tonumber(string.match(text, "^json#(%d+)$"))
		if at == nil then
			error("Can't parse JSON")
		end
		return self.values[at]
	end
	Net._service = self
	return self
end

local function ok(body, status, headers)
	local code = status or 200
	return {
		StatusCode = code,
		StatusMessage = if code < 300 then "OK" else "Bad",
		Success = code < 300,
		Headers = headers or { ["Content-Type"] = "text/plain" },
		Body = body or "",
	}
end

describe("Net", function()
	it("sends a request and reads the response", function()
		local s = service(function()
			return ok("hello")
		end)
		local res = alloy.await(Net.get("https://example.com/a"):header("X-Id", "7"):send()):unwrap()
		expect(res.status).toBe(200)
		expect(res.ok).toBe(true)
		expect(res:text()).toBe("hello")
		expect(res:header("content-type")).toBe("text/plain")
		expect(s.calls[1].Url).toBe("https://example.com/a")
		expect(s.calls[1].Method).toBe("GET")
		expect(s.calls[1].Headers["X-Id"]).toBe("7")
		expect(s.calls[1].Body).toBeNil()
	end)

	it("writes the query in key order and encodes it", function()
		local s = service(function()
			return ok()
		end)
		alloy.await(Net.get("https://example.com/a?x=1"):query({ tag = { "a b", "c" }, page = 2 }):send())
		expect(s.calls[1].Url).toBe("https://example.com/a?x=1&page=2&tag=a%20b&tag=c")
	end)

	it("sends JSON and reads JSON", function()
		local s = service(function(request)
			return ok(request.Body)
		end)
		local value = alloy
			.await(Net.post("https://example.com/items"):json({ name = "sword" }):send_json())
			:unwrap()
		expect(value.name).toBe("sword")
		expect(s.calls[1].Headers["Content-Type"]).toBe("application/json")
		expect(s.calls[1].Headers.Accept).toBe("application/json")
		expect(Net.from_json(Net.to_json({ 1, 2 }):unwrap()):unwrap()).toEqual({ 1, 2 })
		expect(Net.from_json("{").tag).toBe("Err")
	end)

	it("gives Encode and Decode errors as values", function()
		local s = service(function()
			return ok("not json")
		end)
		local encode = alloy.await(Net.post("https://example.com"):json(print):send())
		expect(encode.tag).toBe("Err")
		expect(encode._1.kind).toBe("Encode")
		expect(#s.calls).toBe(0)
		local decode = alloy.await(Net.get("https://example.com"):send()):unwrap():json()
		expect(decode._1.kind).toBe("Decode")
		expect(decode._1.status).toBe(200)
	end)

	it("keeps a bad status as a response until asked", function()
		service(function()
			return ok("gone", 404)
		end)
		local res = alloy.await(Net.get("https://example.com/x"):send()):unwrap()
		expect(res.ok).toBe(false)
		local err = res:error_for_status()._1
		expect(err.kind).toBe("Status")
		expect(err.status).toBe(404)
		expect(err.response.body).toBe("gone")
		expect(tostring(err)).toBe("Net Status: 404 Bad (GET https://example.com/x)")
		local data = alloy.await(Net.get("https://example.com/x"):send_json())
		expect(data._1.kind).toBe("Status")
	end)

	it("joins the base and merges the headers of the client", function()
		local s = service(function()
			return ok()
		end)
		local api = Net.client({ base = "https://api.example.com/v1/", headers = { Authorization = "Bearer a" } })
		alloy.await(api:get("users"):bearer("b"):send())
		alloy.await(api:delete("https://other.example.com/x"):send())
		expect(s.calls[1].Url).toBe("https://api.example.com/v1/users")
		expect(s.calls[1].Headers.Authorization).toBe("Bearer b")
		expect(s.calls[2].Url).toBe("https://other.example.com/x")
		expect(s.calls[2].Method).toBe("DELETE")
		expect(s.calls[2].Headers.Authorization).toBe("Bearer a")
	end)

	it("retries a failed connection and a 503", function()
		local s = service(function(_, call)
			if call == 1 then
				error("HttpError: ConnectFail")
			elseif call == 2 then
				return ok("busy", 503, { ["Retry-After"] = "1" })
			end
			return ok("done")
		end)
		local res = alloy.await(Net.get("https://example.com"):retries(2, 0.1):send()):unwrap()
		expect(res.body).toBe("done")
		expect(#s.calls).toBe(3)
	end)

	it("does not retry a POST on a bad status, and names the last error", function()
		local s = service(function()
			return ok("busy", 503)
		end)
		local res = alloy.await(Net.post("https://example.com"):retries(3):send()):unwrap()
		expect(res.status).toBe(503)
		expect(#s.calls).toBe(1)
		s = service(function()
			error("HttpError: ConnectFail")
		end)
		local err = alloy.await(Net.get("https://example.com"):retries(2, 0.1):send())._1
		expect(err.kind).toBe("Connect")
		expect(err.retryable).toBe(true)
		expect(err.attempts).toBe(3)
	end)

	it("does not retry a host with a certificate this server refuses", function()
		local s = service(function(request)
			if string.find(request.Url, "main.example.com", 1, true) then
				error("HttpError: TlsVerificationFail")
			end
			return ok("from the fallback")
		end)
		local alone = alloy.await(Net.get("https://main.example.com/a"):retries(5):send())._1
		expect(alone.kind).toBe("Tls")
		expect(alone.retryable).toBe(false)
		expect(#s.calls).toBe(1)
		expect(string.find(alone.message, "main.example.com", 1, true) ~= nil).toBe(true)

		local api = Net.client({ base = "https://main.example.com", fallbacks = { "https://alt.example.com" }, retries = 5 })
		s.calls = {}
		expect(alloy.await(api:get("/a"):send()):unwrap().body).toBe("from the fallback")
		expect(#s.calls).toBe(2)
		-- The next request starts at the host that works.
		expect(alloy.await(api:get("/b"):send()):unwrap().url).toBe("https://alt.example.com/b")
		expect(#s.calls).toBe(3)
	end)

	it("names the other transport failures", function()
		local kinds = {
			["HttpError: SslConnectFail"] = "Tls",
			["HttpError: Timedout"] = "Timeout",
			["HttpError: DnsResolve"] = "Connect",
			["Number of requests exceeded limit"] = "Limit",
			["Http requests are not enabled. Enable via game settings"] = "Disabled",
			["HttpService is not allowed to access ROBLOX resources"] = "Blocked",
			["something new"] = "Other",
		}
		for message, kind in kinds do
			service(function()
				error(message)
			end)
			expect(alloy.await(Net.get("https://example.com"):send())._1.kind).toBe(kind)
		end
	end)

	it("says that the service is missing where no game is", function()
		Net._service = nil
		local err = alloy.await(Net.get("https://example.com"):send())._1
		expect(err.kind).toBe("Disabled")
	end)
end)
