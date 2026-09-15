-- A namespace over values: the header opens a table and each member
-- lands on it. Inside the namespace a sibling needs no prefix.
local Math = {}
    const Math_PI = 3.14 Math.PI = Math_PI
    local function Math_clamp(x: number, lo: number, hi: number): number
        return math.max(lo, math.min(hi, x))
    end Math.clamp = Math_clamp
    local function Math_unit(x: number): number
        return Math_clamp(x, 0, 1)
    end Math.unit = Math_unit


print(Math.PI, Math.clamp(5, 0, 3), Math.unit(2))
