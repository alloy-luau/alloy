-- Bindings in conditions. One construct, two spellings, two pattern kinds.

local Players = game:GetService("Players")

local Job = {} Job.__index = Job
    function Job.Build(_1) return setmetatable({ tag = "Build", _1 = _1 }, Job) end
    function Job.Move(_1) return setmetatable({ tag = "Move", _1 = _1 }, Job) end
function Job.is(v) return (type(v) == "table" and getmetatable(v) == Job) end type Job = { tag: "Build", _1: string } | { tag: "Move", _1: Vector3 }

local function first_player(): Player?
    return Players:GetPlayers()[1]
end

-- `if local` binds and tests truthiness. The binding lives in its branch.
-- emits: do local _c1 = first_player() if _c1 then local player = _c1 ... end end
-- The branch declares `player` from the tested temp, so it is `Player`
-- there, not `Player?`, and a closure in the branch sees `Player` too.
do local _c1 = first_player() if _c1 then local player = _c1
    print(player.Name)
end end

-- `const` is the other spelling. The binding cannot be reassigned.
do local _c1 = first_player() if _c1 then local player = _c1
    print(player.UserId)
end end

-- `elseif local` starts a new binding in a new branch.
local function pick(a: Player?, b: Player?): string
    do local _c1 = a if _c1 then local p = _c1
        return p.Name
    else do local _c2 = b if _c2 then local q = _c2
        return q.Name
    else
        return "nobody"
    end end end end
end

-- If-expression form.
local _c1 = first_player() local name = (if _c1 then _c1.Name else "nobody")

-- `in` replaces the default test. Stacked bindings may use earlier ones.
do local _c1 = first_player() if _c1 and (_c1 ~= nil and _c1.AccountAge > 30) then local p = _c1
    print("veteran", p.Name)
end end

do local _c1 = first_player() local _c2 = if _c1 then _c1.Character else nil if _c1 and _c2 then local p, char = _c1, _c2
    print(char:GetPivot())
end end

-- `while local` re-evaluates the binding each iteration.
local queue = { "a", "b", "c" }
while true do local _c1 = table.remove(queue) if not (_c1) then break end local item = _c1
    print(item)
end

-- `not` flips the test. The `else` branch gets the narrowed binding.
-- The `then` block is the guard-clause home.
local function greet()
    local p = first_player() if not (p) then
        return
    end
    print("hi", p.Name)
end

-- A variant pattern tests the tag and binds the payload.
local function run(job: Job)
    do local _c1 = job if (type(_c1) == "table" and _c1.tag == "Build") then local model_name = _c1._1
        print("build", model_name)
    else do local _c2 = job if (type(_c2) == "table" and _c2.tag == "Move") then local target = _c2._1
        print("move to", target)
    end end end end
end

local jobs: { Job } = { Job.Build("house"), Job.Move(Vector3.zero) }
while true do local _c1 = table.remove(jobs) if not ((type(_c1) == "table" and _c1.tag == "Move")) then break end local target = _c1._1
    print(target)
end

-- Destructuring declaration. A mismatch throws with the tag it found.
local _1 = Job.Build("tower") if not (type(_1) == "table" and _1.tag == "Build") then error("pattern `Build(first_name)` did not match, got " .. tostring(if type(_1) == "table" then _1.tag else _1)) end local first_name = _1._1

-- Let-else. The else block must end in return, break, continue, or error.
local function name_of(job: Job): string
    local _1 = job if not (type(_1) == "table" and _1.tag == "Build") then
        return "not a build"
    end local model_name = _1._1
    return model_name
end

print(pick, name, greet, run, first_name, name_of)
