-- The types of a package, as a Luau project that installs it reads them.
local __alloy = require("@alloy") local Named = {} export type Named = { read name: (self: any) -> string } Named.name = (nil :: any) :: (self: any) -> string



local Point = {} Point.__index = Point


 export type Point = typeof(setmetatable({} :: { x: number, y: number }, Point)) setmetatable(Point, { __call = function(_, f: { x: number, y: number? }): Point if f.y == nil then local _y: number = 0 f.y = _y end return ((setmetatable(f, Point) :: any) :: Point) end }) function Point.new(f: { x: number, y: number? }): Point return Point(f) end Point.__tostring = function(s) return __alloy.show_struct("Point", s, { "x", "y" }) end

do
    function Point.name(self: Point): string
        return `({self.x}, {self.y})`
    end
end

local Vec2 = {} Vec2.__index = Vec2 export type Vec2 = typeof(setmetatable({} :: { x: number, y: number }, Vec2)) setmetatable(Vec2, { __call = function(_, f: { x: number, y: number }): Vec2  return ((setmetatable(f, Vec2) :: any) :: Vec2) end }) Vec2.__tostring = function(s) return __alloy.show_struct("Vec2", s, { "x", "y" }) end

do
    function Vec2.new(x: number, y: number): Vec2
        return Vec2({ x = x, y = y })
    end
end

local Color = {} Color.__index = Color Color.Red = ("Red" :: Color)  Color.Green = ("Green" :: Color) function Color.is(v) return (type(v) == "table" and getmetatable(v) == Color) or v == "Red" or v == "Green" end Color.__tostring = function(v) return __alloy.show_variant("Color", v) end export type Color = "Red" | "Green"

local Opt = {} Opt.__index = Opt
Opt.None = ("None" :: Opt<any>)
    function Opt.Some<T>(_1: T): typeof(setmetatable({} :: { tag: "Some", _1: T }, Opt)) return (setmetatable({ tag = "Some", _1 = _1 }, Opt) :: any) end
function Opt.is(v) return (type(v) == "table" and getmetatable(v) == Opt) or v == "None" end Opt.__tostring = function(v) return __alloy.show_variant("Opt", v) end export type Opt<T> = "None" | typeof(setmetatable({} :: { tag: "Some", _1: T }, Opt))

local Tree = {} Tree.__index = Tree
function Tree.Leaf<T>(_1: T): typeof(setmetatable({} :: { tag: "Leaf", _1: T }, Tree)) return (setmetatable({ tag = "Leaf", _1 = _1 }, Tree) :: any) end
    function Tree.Node<T>(_1, _2): typeof(setmetatable({} :: { tag: "Node", _1: Tree<T>, _2: Tree<T> }, Tree)) return (setmetatable({ tag = "Node", _1 = _1, _2 = _2 }, Tree) :: any) end
function Tree.is(v) return (type(v) == "table" and getmetatable(v) == Tree) end Tree.__tostring = function(v) return __alloy.show_variant("Tree", v) end export type Tree<T> = typeof(setmetatable({} :: { tag: "Leaf", _1: T }, Tree)) | typeof(setmetatable({} :: { tag: "Node", _1: Tree<T>, _2: Tree<T> }, Tree))

local Grid = {} Grid.__index = Grid

 export type Grid = typeof(setmetatable({} :: { rows: { number } }, Grid)) setmetatable(Grid, { __call = function(_, f: { rows: { number } }): Grid  return ((setmetatable(f, Grid) :: any) :: Grid) end }) function Grid.new(f: { rows: { number } }): Grid return Grid(f) end Grid.__tostring = function(s) return __alloy.show_struct("Grid", s, { "rows" }) end

do
    function Grid.row(self: Grid, x: number): number?
        return self.rows[x]
    end

    function Grid.shift(self: Grid, r: number): number
        return r + #self.rows
    end

    function Grid.top(self: Grid, x: number): number
        const r = self:row(x)
        return if r then self:shift(r) else 0
    end
end return { Named = Named, Point = Point, Vec2 = Vec2, Color = Color, Opt = Opt, Tree = Tree, Grid = Grid }
