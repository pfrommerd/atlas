
// #[test]
// fn test_add() {
//     let store = LocalStorage::new_default();
//     let cache = ForceCache::new();
//     let roots = SIMPLE_ADD.get().unwrap().unpack_into(&store).unwrap();

//     let thunk = roots[1].clone();
//     // the machine state has to outlive the executor
//     let machine = Machine::new(&store, &cache);
//     let exec = LocalExecutor::new();
//     future::block_on(exec.run(async {
//         let res = machine.force(thunk.clone()).await.unwrap();
//         let val = res.value().unwrap();
//         let v = val.reader().numeric().unwrap();
//         assert_eq!(v, Numeric::Int(3))
//     }));
// }