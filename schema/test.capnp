@0xc423d32339980933;

using import "op.capnp".Code;
# Test heaps
const simpleAdd :Code = (
    params=[],
    externals=[
        (dest=(id=0, usedBy=[0]), ptr=100),
        (dest=(id=1, usedBy=[0]), ptr=101)
    ],
    ops=[
        (builtin=(dest=(id=2, usedBy=[1]), op="add", args=[0, 1])),
        (ret=2)
    ]
);