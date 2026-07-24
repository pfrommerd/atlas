The typing system of the atlas language (and by extension, the core language) is a bit special.

Although things are statically typed, these hints are evaluated at runtime.

For instance, consider the function (in atlas)

sum (x: Int) (y: Int) = x + y

The : Int type hint is held in a special "TypeProjection" Term that contains the address of both the value expression and the type expression.

When evaluated, it forces both the value expression and the type expression (since type expressions are just regular expressions!) to whnf, and then tries to apply the projection.

In our type system, all values and types have 4 properties

- An index table, mapping indices to their associated values. This is used for e.g. tuple destructuring.
- A property table, mapping strings to their associated values. This is useful for e.g. field access (foo.bar)
- An operator table, mapping operators to their associated functions (or in the case of unary operators, the resulting values)

Atlas can do both runtime and static type checking: 
 - At runtime, operations like x.foo (or operators on types that do not support those operations) fill convert into error types as usual.
   Projections, when evaluated, also convert into error types if their projections are not met.
 - We should have a special evaluation mode that resolves only the minimal information needed in order to ensure the typing constraints are met.
   This involves decomposing operators into a "type-checked" version of the operator, and a type projection. For instance, the core-style lambda

   \x -> x.foo would get converted into \x -> (x : HasField "foo").foo
   and \ x y -> x + y gets converted to \x -> (x: HasAdd) + (y: HasAdd)

   type projections then get get pushed downwards until they annihilate either
   with the type of a whnf variable, or if the meet another projection. For instance,


