--[[
Type declarations cut from jecs 0.11, `src/jecs.luau`
(https://github.com/ukendio/jecs): the `World` type with its `query`
member alone, and the types that member names. No code of jecs is
here. The test `an_append_of_a_function_over_a_large_type_checks` in
`tests/analyze.rs` reads this file: a copy of `World` that a type
function returns does not equal `World` to the checker.

MIT License

Copyright (c) 2024 jecs authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
]]

type Column = { any }
export type Archetype = {
	columns_map: { [Component]: Column }
}
export type Entity<T = nil> = { __T: T }
export type Id<T = any> = { __T: T }
export type Component<T=any> = { __T: T }
export type Iter<T...> = (query: Query<T...>) -> () -> (Entity, T...)
export type Cached_Query_Iter<T...> = (query: Cached_Query<T...>) -> () -> (Entity, T...)
export type Cached_Query<T...> = typeof(setmetatable(
	{} :: {
		iter: Cached_Query_Iter<T...>,
		archetypes: (Cached_Query<T...>, override: boolean?) -> { Archetype },
		has: (Cached_Query<T...>, Entity) -> boolean,
		fini: (Cached_Query<T...>) -> (),

		ids: { Id<any> },
		filter_with: { Id<any> }?,
		filter_without: { Id<any> }?,
		archetypes_map: { [number]: number },
	},
	{} :: {
		__iter: Cached_Query_Iter<T...>,
	})
)
export type Query<T...> = typeof(setmetatable(
	{} :: {
		iter: Iter<T...>,
		with: ((Query<T...>, ...Component) -> Query<T...>),
		without: ((Query<T...>, ...Component) -> Query<T...>),
		archetypes: (Query<T...>) -> { Archetype },
		cached: (Query<T...>) -> Cached_Query<T...>,
		has: (Query<T...>, Entity) -> boolean,
		ids: { Id<any> },
		filter_with: { Id<any> }?,
		filter_without: { Id<any> }?,
	},
	{} :: {
		__iter: Iter<T...>,
	}
))
export type World = {
	query: ((World) -> Query<nil>)
		& (<A>(World, Component<A>) -> Query<A>)
		& (<A, B>(World, Component<A>, Component<B>) -> Query<A, B>)
		& (<A, B, C>(World, Component<A>, Component<B>, Component<C>) -> Query<A, B, C>)
		& (<A, B, C, D>(World, Component<A>, Component<B>, Component<C>, Component<D>) -> Query<A, B, C, D>)
		& (<A, B, C, D, E>(World, Component<A>, Component<B>, Component<C>, Component<D>, Component<E>) -> Query<A, B, C, D, E>)
		& (<A, B, C, D, E, F>(World, Component<A>, Component<B>, Component<C>, Component<D>, Component<E>, Component<F>) -> Query<A, B, C, D, E, F>)
		& (<A, B, C, D, E, F, G>(
			World,
			Component<A>,
			Component<B>,
			Component<C>,
			Component<D>,
			Component<E>,
			Component<F>,
			Component<G>
		) -> Query<A, B, C, D, E, F, G>)
		& (<A, B, C, D, E, F, G, H>(
			World,
			Component<A>,
			Component<B>,
			Component<C>,
			Component<D>,
			Component<E>,
			Component<F>,
			Component<G>,
			Component<H>,
			...Component<any>
		) -> Query<A, B, C, D, E, F, G, H>),
}
return {}
