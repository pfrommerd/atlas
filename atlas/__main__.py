from .type import typed, construcotr, variaable, method, record, list

my_record = record(name=str)

@typed('test')
class Test:
    @constructor
    def __init__(self, name):
        self.name = name
        self.foo = foo

    # Annotate with a variable 'foo'
    # that maps actually to 'x'
    @variable('foo', str, 'x')
    # Annotate with a variable 'bar'
    # that maps actually to 'y'
    @variable('bar', str, 'y')

    @method('do_something')
    def do_something(arg : str, my_arg : ('second_arg', int)) -> None:
        pass
