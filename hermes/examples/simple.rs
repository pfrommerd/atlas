use smol::LocalExecutor;

use futures_lite::future;

#[hermes::service]
pub trait Hello {
    async fn hello(&self, name: String) -> String;
}

struct World;

#[hermes::implement]
impl Hello for World {
    async fn hello(&self, name: String) -> String {
        println!("Received: {name}");
        format!("Hello Word, {name}")
    }
}

fn main() {
    let handle = HelloHandle::new(World);
    let executor = LocalExecutor::new();
    future::block_on(executor.run(async {
        let res = handle.hello(String::from("Foo")).await.unwrap();
        println!("Response: {res}");
    }));
}
