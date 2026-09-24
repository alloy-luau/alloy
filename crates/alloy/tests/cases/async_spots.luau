-- Where an `await` may stand: the forms that own their thread.
local __alloy = require("@alloy") local function rank(n: number): __alloy.Future<number> return __alloy.future(function(): number
    return n
end) end

local function best(xs: __alloy.Array<number>): __alloy.Future<number> return __alloy.future(function(): number
    local top = __alloy.await(rank(xs[1]))

    local pick = function(n: number): __alloy.Future<number> return __alloy.future(function(): number
        return __alloy.await(rank(n))
    end) end

    return __alloy.await(pick(top))
end) end

-- An `async do` block stands alone as a statement.
__alloy.future(function()
    print(__alloy.await(rank(1)))
end)

-- And inside a plain function, where it is the fix for a bare `await`.
local function warm()
    __alloy.future(function()
        print(__alloy.await(rank(2)))
    end)
end

-- `after` runs its block on the timer's own thread.
task.delay(1, function()
    print(__alloy.await(rank(3)))
end)

local held = __alloy.future(function()
    return __alloy.await(rank(4))
end)

print(best, warm, held)
