-- A `??` in a loop's `where` reads the loop's names, so its temp goes
-- after `do`, where the loop binds them (LANG_BUGS 91).
export type Extra = { layer: { name: string } }

local function run(extras: { Extra }, layers: { [string]: number }, place: (Extra) -> ()): ()
  for _, extra in extras do local _1 = layers[extra.layer.name] if not (((if _1 == nil then 0 else _1)) > 0) then continue end
    place(extra)
  end
end return { run = run }
