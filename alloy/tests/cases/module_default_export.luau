-- One `export default` and nothing else. The table the module returns
-- carries it under `default`, the name a bare import reads.
local function make(): number
    return 1
end return { default = make }
