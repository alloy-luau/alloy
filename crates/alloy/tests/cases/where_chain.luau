-- A `where` before `;` tests its own binding, and the next binding runs
-- only when that test holds (LANG_BUGS 90).
local function held(root: Instance): number
  do local _c1 = root:FindFirstChild('A') local _c2 = if _c1 and ((typeof(_c1) == "Instance" and (_c1 :: Instance):IsA("Frame"))) then _c1:FindFirstChild('B') else nil if _c1 and ((typeof(_c1) == "Instance" and (_c1 :: Instance):IsA("Frame"))) and _c2 then local a, b = _c1, _c2
    return 1
  end end

  return 0
end

local function pair(t: { [string]: number }): number
  do local _c1 = t.a local _c2 = if _c1 and (_c1 > 0) then t.b else nil if _c1 and (_c1 > 0) and _c2 and (_c2 > _c1) then local a, b = _c1, _c2
    return a + b
  end end

  return 0
end return { held = held, pair = pair }
