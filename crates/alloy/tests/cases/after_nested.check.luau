local __alloy = require("@alloy") local armed = false

local function schedule(): __alloy.Future<nil> return (__alloy.future(function()
    task.delay(1, function()
        task.delay(2, function() if armed then
            print("deep")
        end end)

        print("shallow")
    end)

    __alloy.await(__alloy.Future.delay(1))
end) :: any) :: __alloy.Future<nil> end

armed = true

task.delay(0, function()
    for i = 1, 3 do
        if i == 2 then
            return
        end
    end
end)

print(schedule, armed)
