-- A `try` inside a `try do` block leaves the block through the block's
-- `fail`, so the block's own `return` decides the value type.
local __alloy = require("@alloy") local function parse(s: string): __alloy.Result<number, string>
    return __alloy.Ok(1)
end

local function code(s: string): __alloy.Result<string, number>
    return __alloy.Ok("x")
end

local plain = __alloy.try_block(function(__fail) return 1 end)

local through = __alloy.try_block(function(__fail) return parse("7") end)

local one = __alloy.try_block(function(__fail)
    local _1 = parse("1") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local v = _1._1
    return v + 1
end)

-- Two `try`s of one error type keep it; the payload passes as written.
local same = __alloy.try_block(function(__fail)
    local _1 = parse("1") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local a = _1._1
    _1 = parse("2") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local b = _1._1
    return a + b
end)

-- Two error types this file can name as different pass `any`, so the
-- error type stays open instead of reporting the second one.
local mixed = __alloy.try_block(function(__fail)
    local _1 = parse("1") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local a = _1._1
    _1 = code("2") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local b = _1._1
    return a + #b
end)

-- A block inside a block gets a `fail` of its own.
local nested = __alloy.try_block(function(__fail)
    local inner = __alloy.try_block(function(__fail2)
        local _1 = parse("3") if _1.tag == "Err" then __fail2(_1._1, _1.trace) end local v = _1._1
        return v * 2
    end)
    return inner:unwrap_or(0)
end)

-- `return Err(e)` is the block failing with `e`, which is what `fail`
-- says. A `return` of it would decide the closure's value type.
local guarded = __alloy.try_block(function(__fail)
    local _1 = parse("1") if _1.tag == "Err" then __fail(_1._1, _1.trace) end local v = _1._1
    if v > 10 then
        __fail("too big", nil)
    end
    return v
end)

-- A `try` inside a function inside a block still returns from the
-- function.
local function reader(): __alloy.Result<number, string>
    local _1 = parse("4") if _1.tag == "Err" then return _1 end local v = _1._1
    return __alloy.Ok(v)
end

local outer = __alloy.try_block(function(__fail)
    return reader()
end)

print(plain, through, one, same, mixed, nested, guarded, outer)
