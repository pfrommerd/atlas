@0xc423d32339980933;

using import "value.capnp".PackedHeap;
# Test heaps
const simpleAdd :PackedHeap = (
entries=[
    (loc=0, val=(code=(
        params=[],
        externals=[
            (dest=(id=0, usedBy=[0]), ptr=100),
            (dest=(id=1, usedBy=[0]), ptr=101)
        ],
        ops=[
            (builtin=(dest=(id=2, usedBy=[1]), op="add", args=[0, 1])),
            (ret=2)
        ]
    ))),
    (loc=1, val=(thunk=0)),

    # constants
    (loc=100, val=(primitive=(int=1))),
    (loc=101, val=(primitive=(int=2)))
],
roots=[0, 1]
);
