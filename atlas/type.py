import hashlib


# A type is just a function
class Type:
    def __init__(self, identifier, templates={}, args, executor):
        self.identifier = identifier
        self.templates = templates

        self.args = args
        self.executor = executor

        self.attributes = {}; # map of string -> (type, getter)

    def add_attribute(self, name, type, getter):
        self.attributes[name] = (type, getter)

    def call(self, obj_, args):
        return self.executor(obj_, args)

    @property
    def id(self):
        return hashlib.md5(self.identifier)

# ------- Builtin types -------

none_type = Type('none')

int_type = Type('int')

float_type = Type('float')

string_type = Type('string')

def list(other_type):
    ot = boxed_type(other_type)

    t = Type('list', {'elements': ot});

    # the methods
    def append(list, element):
        list.append(element)

    t.add_method('append', none_type, {'element': ot}, append)

def record(**kwargs):
    types = {name : boxed_type(t) for (name, t) in kwargs.items()}

    # use the record types as the templates
    t = Type('record', types)
    for (name, type) in types.items():
        def get(x):
            return x[name]
        def set(x, val):
            x[name] = val
        t.add_variable(name, type, get, set)
    return t;

# The 'type' of an attribute is determined
# by its type
def attr(name, attr_type, map=None):


""" Pass in a python class, get back a type of the class """
def boxed_type(t):
    if t is None:
        return none_type
    elif t is str:
        return string_type
    elif t is int:
        return int_type
    elif t is float:
        return float_type
    elif isinstance(t, Type):
        return t
    elif t._atlas_type:
        return t._atlas_type

# -------------- Decorators -------------------

# Decorators for the members of a type
# will get collected on typed() annotation of the class
def attribute(func, name, type, map=None):
    if (not hasattr(func, '_atlas_extras')):
        func._atlas_extras = []
    # Add an extra type to this function
    return func

def variable(func, name, type, map=None):
    if (not hasattr(func, '_atlas_vars')):
        func._atlas_vars = []
    func._atlas_vars.append((name, type, map))
    return func

def method(func, name):
    if (not hasattr(func, '_atlas_methods')):
        func._atlas_methods = []
    func._atlas_methods.append((name, func))
    return func

def constructor(func):
    func._is_atlas_constructor = True

# Type decorator to create a new type
# associated with a particular class
def typed(cls, name):
    t = Type(name)

    constructor_defs = None
    method_defs = []
    variable_defs = []
    # now gather all the members

    cls._atlas_type = t
    return cls
