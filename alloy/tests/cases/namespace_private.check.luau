-- `private` keeps a member inside its namespace. The table still
-- carries it, so a sibling reaches it; the check is the compiler's.
local Counter = {}
    local Counter_total = 0
    local function Counter_bump(): number
        Counter_total += 1

        return Counter_total
    end Counter.bump = Counter_bump


print(Counter.bump())
