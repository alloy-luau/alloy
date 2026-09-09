local __alloy = require("@alloy") local Timer = {} Timer.__index = Timer setmetatable(Timer, { __call = function(_, f)  return setmetatable(f, Timer) end }) function Timer.new(f) return Timer(f) end

 type Timer = typeof(setmetatable({} :: { left: number }, Timer)) Timer.__tostring = function(s) return __alloy.show_struct("Timer", s, { "left" }) end

do
    function Timer.destroy(self)
        self.left = 0
    end
end

local part = Instance.new("Part")
__alloy.destroy(part)

local timer = Timer({ left = 5 })
__alloy.destroy(timer)

game:GetService("Debris"):AddItem(part, 3)
task.delay(1.5, function() timer:destroy() end)

function drop(x)
    __alloy.destroy_after(x, 4)
end

task.delay(2, function()
    print("late")
end)

local ready = false

task.delay(0, function() if ready then
    print("gated")
end end)

task.delay(1, function()
    if ready then
        return
    end

    print("still here")
end)

print(drop, part, timer)
