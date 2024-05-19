// #[hermes::service]
// trait Hello {
//     async fn world(&self) -> Result<String>;
// }

// This generates to (without the Gen prefix)
use hermes::Result;

trait Hello where Self : Sized {
    async fn world(&self) -> Result<String>;
}

trait HelloDyn {
    fn world<'s>(&'s self) -> 
        core::pin::Pin<Box<dyn core::future::Future<Output=Result<String>> + 's>>;
}
// blanket implementation
impl<T> HelloDyn for T where T: Hello {
    fn world<'s>(&'s self) -> 
            core::pin::Pin<Box<dyn core::future::Future<Output=Result<String>> + 's>> {
        Box::pin(async move {
            self.world().await
        })
    }
}

trait Service {
    type Dyn : ?Sized;
}

impl<T> Service for T where T : Hello {
    type Dyn = dyn HelloDyn;
}

enum Handle<T> {
    Rc(Rc<T>)
}

// note that the handle itself implements the service
impl Hello for HelloHandle {
    async fn world(&self) -> Result<String> {
        self.0.world().await
    }
}


fn main() {

}

// struct HelloWorld;

// #[hermes::implement]
// impl Hello for HelloWorld {
//     async fn world(&self) -> String {
//         String::from("Hello World")
//     }
// }